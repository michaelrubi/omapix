//! The live session: a running Omapix listens on a Unix socket,
//! `$XDG_RUNTIME_DIR/omapix.sock`, for the same JSON-RPC (MCP) messages as
//! `omapix --mcp`, but acting on the image open in the window: each tool
//! call is passed to the app, which runs it in `poll` through `Command` as
//! a menu or shortcut would.
//!
//! It also keeps Omapix to one window: `omapix a.tif` while one is running
//! hands the paths to it (`open_documents`), where they open as tabs, and
//! exits. A `--round-trip` launch stays connected until the running Omapix
//! quits, since darktable imports the TIFF once the launch has ended.
//!
//! Only this user can reach the socket: `$XDG_RUNTIME_DIR` is theirs alone.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value, json};

use crate::commands::Command;
use crate::mcp::{respond, serve_lines};

/// What an agent or a later launch asks the app to do.
#[derive(Debug)]
pub enum Request {
    /// Open each as a tab, as a round trip from darktable if so.
    Open { paths: Vec<PathBuf>, round_trip: bool },
    /// Run a command, as its menu item would.
    Run(Command),
    /// Report the image showing, as `inspect_document` does a file.
    Inspect,
}

/// A request, and where its answer goes.
pub type Asked = (Request, Sender<Result<Value, String>>);

/// Where the socket is, if there's a runtime directory to put it in.
pub fn socket() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("omapix.sock"))
}

/// Hand `paths` to the Omapix listening at `socket`. True once it has
/// taken them; for a round trip, also once it has quit.
pub fn hand_off(socket: &Path, paths: &[PathBuf], round_trip: bool) -> bool {
    let Ok(stream) = UnixStream::connect(socket) else {
        return false;
    };
    let paths: Vec<_> = paths.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
    let call = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "open_documents", "arguments": { "paths": paths, "round_trip": round_trip } },
    });
    if writeln!(&stream, "{call}").is_err() {
        return false;
    }
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    let taken = reader.read_line(&mut line).is_ok() && serde_json::from_str::<Value>(&line).is_ok_and(|reply| reply["result"]["isError"] == false);
    if taken && round_trip {
        // Until the running Omapix quits, closing the socket.
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    }
    taken
}

/// Listen at `socket`, passing requests on with a repaint of `ctx` so
/// they're picked up. `None` if another Omapix is listening there, or it
/// can't be made. A socket left by one that crashed is replaced.
pub fn listen(socket: &Path, ctx: &egui::Context) -> Option<Receiver<Asked>> {
    if UnixStream::connect(socket).is_ok() {
        log::info!("another Omapix is listening at {}", socket.display());
        return None;
    }
    let _ = std::fs::remove_file(socket);
    let listener = match UnixListener::bind(socket) {
        Ok(listener) => listener,
        Err(e) => {
            log::warn!("{}: {e}", socket.display());
            return None;
        }
    };
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                serve_lines(BufReader::new(&stream), &stream, |line| {
                    respond(line, tools, |name, args| ask(name, args, &tx, &ctx))
                })
            });
        }
    });
    Some(rx)
}

/// Pass a tool call to the app and wait for its answer; `None` if there's
/// no such tool.
fn ask(name: &str, args: &Value, tx: &Sender<Asked>, ctx: &egui::Context) -> Option<Result<Value, String>> {
    let request = match name {
        "open_documents" => match args["paths"].as_array() {
            Some(paths) => Ok(Request::Open {
                paths: paths.iter().filter_map(|p| p.as_str()).map(PathBuf::from).collect(),
                round_trip: args["round_trip"].as_bool().unwrap_or(false),
            }),
            None => Err("missing \"paths\"".to_owned()),
        },
        "run_command" => {
            let command = args["command"].as_str().unwrap_or_default();
            Command::from_name(command)
                .map(Request::Run)
                .ok_or_else(|| format!("there's no command \"{command}\""))
        }
        "inspect_document" => Ok(Request::Inspect),
        _ => return None,
    };
    Some(request.and_then(|request| {
        let (reply, answer) = channel();
        tx.send((request, reply)).map_err(|_| "Omapix is closing".to_owned())?;
        ctx.request_repaint();
        answer.recv().map_err(|_| "Omapix is closing".to_owned())?
    }))
}

fn tools() -> Value {
    json!([
        {
            "name": "inspect_document",
            "description": "Report the image showing in the Omapix window: its size, colour profile, bit depth and layers (top first: name, kind, blend mode, opacity, visibility, mask, clipping, group).",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "run_command",
            "description": "Run an Omapix command on the image showing, as its menu item or shortcut does, e.g. NewLayer, FrequencySeparation (which opens its dialog), Undo, Save. Names are those of the Command enum in crates/omapix/src/commands.rs.",
            "inputSchema": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
            },
        },
        {
            "name": "open_documents",
            "description": "Open images in the Omapix window, a tab each.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Absolute paths" },
                    "round_trip": { "type": "boolean", "description": "Open as a round trip from darktable: saving also writes the TIFF" },
                },
                "required": ["paths"],
            },
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket_in(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omapix-bridge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("omapix.sock")
    }

    /// Answer every request with `Ok`, as an app would, keeping them.
    fn answer(rx: Receiver<Asked>) -> std::thread::JoinHandle<Vec<Request>> {
        std::thread::spawn(move || {
            let mut asked = Vec::new();
            while let Ok((request, reply)) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
                let _ = reply.send(Ok(json!({})));
                asked.push(request);
            }
            asked
        })
    }

    #[test]
    fn a_later_launch_hands_its_paths_to_the_one_listening() {
        let socket = socket_in("hand-off");
        let rx = listen(&socket, &egui::Context::default()).unwrap();
        // A second Omapix doesn't take the socket over.
        assert!(listen(&socket, &egui::Context::default()).is_none());
        let asked = answer(rx);
        assert!(hand_off(&socket, &[PathBuf::from("a.tif"), PathBuf::from("/photos/b.tif")], false));
        let asked = asked.join().unwrap();
        let Some(Request::Open { paths, round_trip: false }) = asked.first() else {
            panic!("{asked:?}");
        };
        assert_eq!(paths, &[std::env::current_dir().unwrap().join("a.tif"), PathBuf::from("/photos/b.tif")]);
        std::fs::remove_dir_all(socket.parent().unwrap()).ok();
    }

    #[test]
    fn a_socket_left_by_a_crash_is_replaced() {
        let socket = socket_in("stale");
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        // Nothing's listening, so there's no one to hand off to.
        assert!(!hand_off(&socket, &[PathBuf::from("/a.tif")], false));
        let asked = answer(listen(&socket, &egui::Context::default()).unwrap());
        assert!(hand_off(&socket, &[PathBuf::from("/a.tif")], false));
        assert_eq!(asked.join().unwrap().len(), 1);
        std::fs::remove_dir_all(socket.parent().unwrap()).ok();
    }

    #[test]
    fn agents_get_the_tools_and_unknown_commands_are_refused_before_the_app() {
        let socket = socket_in("tools");
        let asked = answer(listen(&socket, &egui::Context::default()).unwrap());
        let stream = UnixStream::connect(&socket).unwrap();
        let mut reader = BufReader::new(&stream);
        let mut send = |message: Value| {
            writeln!(&stream, "{message}").unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        };
        let list = send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }));
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 3);
        let bad = send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "run_command", "arguments": { "command": "Juggle" } } }));
        assert_eq!(bad["result"]["isError"], true);
        let good =
            send(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "run_command", "arguments": { "command": "NewLayer" } } }));
        assert_eq!(good["result"]["isError"], false);
        drop(stream);
        assert!(matches!(asked.join().unwrap()[..], [Request::Run(Command::NewLayer)]));
        std::fs::remove_dir_all(socket.parent().unwrap()).ok();
    }
}
