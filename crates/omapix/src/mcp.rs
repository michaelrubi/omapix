//! `omapix --mcp`: a Model Context Protocol server on standard input and
//! output (one JSON-RPC 2.0 message a line), so AI agents can inspect,
//! retouch and export images without a window. Retouching runs the same
//! steps as `OMAPIX_SCRIPT` on a windowless [`App`]; files are read and
//! written by the engine. Logs go to standard error.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use omapix_engine::{Document, export, io, ora, psd};
use serde_json::{Value, json};

use crate::app::{App, JPEG_QUALITY};
use crate::settings::FilterSettings;

/// What `add_adjustment_layer` adds: Layer › New Adjustment Layer's
/// commands, without their `New`.
const ADJUSTMENTS: [&str; 7] = [
    "Curves",
    "Levels",
    "HueSaturation",
    "ColorBalance",
    "SelectiveColor",
    "ChannelMixer",
    "ColorLookup",
];

/// The protocol versions understood, newest last.
const VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// Answer requests from `input` on `output` until `input` ends.
pub fn serve(input: impl BufRead, output: impl Write) {
    let (filters, defaults) = FilterSettings::load();
    let mut server = Server {
        app: App::headless(filters.in_memory(), defaults),
        ctx: egui::Context::default(),
    };
    serve_lines(input, output, |line| server.handle(line));
}

/// Answer each line of `input` on `output` with `handle`, until `input`
/// ends (here and on the live session's socket).
pub(crate) fn serve_lines(input: impl BufRead, mut output: impl Write, mut handle: impl FnMut(&str) -> Option<Value>) {
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(&line)
            && writeln!(output, "{reply}").and_then(|_| output.flush()).is_err()
        {
            break;
        }
    }
}

struct Server {
    app: App,
    ctx: egui::Context,
}

/// A JSON-RPC error: its code and message.
type RpcError = (i64, String);

/// The reply to one JSON-RPC line, or `None` for a notification: the
/// protocol's own methods, and `tools/call` through `call`, which gives
/// `None` for a tool it doesn't have. A tool that fails says why in its
/// result, as the protocol asks; only an unknown tool is a JSON-RPC error.
pub(crate) fn respond(line: &str, tools: impl FnOnce() -> Value, call: impl FnOnce(&str, &Value) -> Option<Result<Value, String>>) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(e) => return Some(reply(Value::Null, Err((-32700, format!("Parse error: {e}"))))),
    };
    let id = message.get("id").cloned()?;
    let Some(method) = message["method"].as_str() else {
        return Some(reply(id, Err((-32600, "Invalid request".into()))));
    };
    let result = match method {
        "initialize" => {
            let asked = message["params"]["protocolVersion"].as_str();
            let version = asked.filter(|v| VERSIONS.contains(v)).unwrap_or(VERSIONS[VERSIONS.len() - 1]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "omapix", "version": env!("CARGO_PKG_VERSION") },
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap_or_default();
            match call(name, &message["params"]["arguments"]) {
                Some(Ok(value)) => Ok(json!({
                    "content": [{ "type": "text", "text": value.to_string() }],
                    "structuredContent": value,
                    "isError": false,
                })),
                Some(Err(e)) => Ok(json!({
                    "content": [{ "type": "text", "text": e }],
                    "isError": true,
                })),
                None => Err((-32602, format!("Unknown tool: {name}"))),
            }
        }
        _ => Err((-32601, format!("Method not found: {method}"))),
    };
    Some(reply(id, result))
}

impl Server {
    fn handle(&mut self, line: &str) -> Option<Value> {
        respond(line, tools, |name, args| self.call(name, args))
    }

    /// Run a tool, or `None` if there's no such tool.
    fn call(&mut self, name: &str, args: &Value) -> Option<Result<Value, String>> {
        Some(match name {
            "inspect_document" => path(args, "path").and_then(|p| load(&p)).map(|doc| inspect(&doc)),
            "apply_frequency_separation" => {
                if let Some(radius) = args["radius"].as_f64() {
                    self.app.filters_mut().separation_radius = Some(radius as f32);
                }
                self.retouch(args, &["FrequencySeparation"])
            }
            "apply_auto_retouch" => match args["preset"].as_str() {
                Some(preset) => self.retouch(args, &[&format!("AutoRetouch {preset}")]),
                None => self.retouch(args, &["AutoRetouch"]),
            },
            "apply_denoise" => {
                let filters = self.app.filters_mut();
                if let Some(amount) = args["luminance"].as_f64() {
                    filters.denoise_luminance = amount.clamp(0.0, 100.0) as f32;
                }
                if let Some(amount) = args["color"].as_f64() {
                    filters.denoise_color = amount.clamp(0.0, 100.0) as f32;
                }
                self.retouch(args, &["Denoise"])
            }
            "add_adjustment_layer" => match args["kind"].as_str() {
                Some(kind) if ADJUSTMENTS.contains(&kind) => self.retouch(args, &[&format!("New{kind}")]),
                _ => Err(format!("\"kind\" should be one of {}", ADJUSTMENTS.join(", "))),
            },
            "get_settings" => serde_json::to_value(FilterSettings::load().0).map_err(|e| e.to_string()),
            "update_settings" => (|| {
                let (mut settings, _) = FilterSettings::load();
                settings.update(&args["settings"])?;
                *self.app.filters_mut() = settings.clone().in_memory();
                serde_json::to_value(settings).map_err(|e| e.to_string())
            })(),
            "export_document" => (|| {
                let doc = load(&path(args, "path")?)?;
                let output = path(args, "output")?;
                write(&doc, &output, args)?;
                Ok(json!({ "output": output }))
            })(),
            _ => return None,
        })
    }

    /// Open `path`, run script `steps` on it and write it to `output`.
    fn retouch(&mut self, args: &Value, steps: &[&str]) -> Result<Value, String> {
        let doc = load(&path(args, "path")?)?;
        let output = path(args, "output")?;
        let (doc, message) = self.app.run_steps(doc, steps, &self.ctx)?;
        write(&doc, &output, args)?;
        let mut value = inspect(&doc);
        value["output"] = json!(output);
        value["message"] = json!(message);
        Ok(value)
    }
}

fn reply(id: Value, result: Result<Value, RpcError>) -> Value {
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    }
}

fn path(args: &Value, key: &str) -> Result<PathBuf, String> {
    args[key].as_str().map(PathBuf::from).ok_or_else(|| format!("missing \"{key}\""))
}

fn load(path: &Path) -> Result<Document, String> {
    io::load(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write `doc` as its extension says, as File › Save As and the exports do.
fn write(doc: &Document, path: &Path, args: &Value) -> Result<(), String> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let written = match ext.as_deref() {
        Some("ora") => ora::save(doc, path),
        Some("tif" | "tiff") => export::tiff(doc, path),
        Some("jpg" | "jpeg") => export::jpeg(doc, path, args["quality"].as_u64().map_or(JPEG_QUALITY, |q| q.clamp(1, 100) as u8)),
        Some("png") => export::png(doc, path),
        Some("psd" | "psb") => psd::save(doc, path),
        _ => return Err(format!("{}: can't write this kind of file (.ora, .tif, .jpg, .png or .psd)", path.display())),
    };
    written.map_err(|e| format!("{}: {e}", path.display()))
}

/// The size, colour profile and layers of `doc`, top layer first.
pub(crate) fn inspect(doc: &Document) -> Value {
    let layers: Vec<Value> = doc
        .layers
        .iter()
        .rev()
        .map(|l| {
            let kind = match &l.adjustment {
                Some(a) => a.name(),
                None if l.is_group => "group",
                None => "pixels",
            };
            json!({
                "id": l.id,
                "name": l.name,
                "kind": kind,
                "blend": l.blend.name(),
                "opacity": l.opacity,
                "visible": l.visible,
                "mask": l.mask.is_some(),
                "clipped": l.clipped,
                "parent": l.parent,
            })
        })
        .collect();
    json!({
        "width": doc.width,
        "height": doc.height,
        "profile": doc.profile.description(),
        "bits": doc.source_bits,
        "layers": layers,
    })
}

fn tools() -> Value {
    let path = json!({ "type": "string", "description": "The image: .ora, .tif, .psd, .jpg or .png" });
    let output = json!({ "type": "string", "description": "Where to write the result; its extension (.ora, .tif, .jpg, .png or .psd) chooses the format" });
    let quality = json!({ "type": "integer", "minimum": 1, "maximum": 100, "description": "JPEG quality (92 if left out)" });
    json!([
        {
            "name": "inspect_document",
            "description": "Open an image and report its size, colour profile, bit depth and layers (top first: name, kind, blend mode, opacity, visibility, mask, clipping, group).",
            "inputSchema": { "type": "object", "properties": { "path": path }, "required": ["path"] },
        },
        {
            "name": "apply_frequency_separation",
            "description": "Split the top layer into a tone layer and a Grain Merge texture layer, as Retouch › Frequency Separation does, and write the result (as .ora to keep the layers).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "output": output,
                    "radius": { "type": "number", "description": "Blur radius in pixels (as last used, or worked out from the image's size, if left out)" },
                },
                "required": ["path", "output"],
            },
        },
        {
            "name": "apply_auto_retouch",
            "description": "Find the faces and build Auto Retouch's layers for each (healed blemishes, smoothed skin, even tone, dodge and burn), as Retouch › Auto Retouch does, and write the result. Needs the AI models (scripts/fetch-models.sh).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "output": output,
                    "preset": { "type": "string", "enum": ["Natural", "Standard", "Strong"], "description": "As last used if left out" },
                },
                "required": ["path", "output"],
            },
        },
        {
            "name": "apply_denoise",
            "description": "Remove noise with the AI model onto a new Denoise layer above the top layer, as Filter › Noise › Denoise does, and write the result. Needs the denoise model (scripts/fetch-models.sh).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "output": output,
                    "luminance": { "type": "number", "minimum": 0, "maximum": 100, "description": "How much luminance noise goes, in % (as last used if left out)" },
                    "color": { "type": "number", "minimum": 0, "maximum": 100, "description": "How much colour noise goes, in % (as last used if left out)" },
                },
                "required": ["path", "output"],
            },
        },
        {
            "name": "add_adjustment_layer",
            "description": "Add an adjustment layer with its default settings (no change yet, to adjust in Omapix) above the top layer, as Layer › New Adjustment Layer does, and write the result (as .ora to keep it a layer).",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path, "output": output, "kind": { "type": "string", "enum": ADJUSTMENTS } },
                "required": ["path", "output", "kind"],
            },
        },
        {
            "name": "get_settings",
            "description": "The filter and retouching settings Omapix remembers (~/.config/omapix/filters.toml): each filter's and retouching setup's settings as last used, which the apply tools use too.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "update_settings",
            "description": "Change some of the settings get_settings shows and save them. Names must be ones get_settings shows; a section can be given in part, e.g. {\"unsharp_mask\": {\"amount\": 1.2}}. Omapix windows already open pick them up when restarted.",
            "inputSchema": {
                "type": "object",
                "properties": { "settings": { "type": "object", "description": "The settings to change, by name" } },
                "required": ["settings"],
            },
        },
        {
            "name": "export_document",
            "description": "Write an image in another format: .ora keeps the layers; .tif (16-bit), .jpg, .png (8-bit sRGB) and .psd flatten or convert them as File › Export does.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path, "output": output, "quality": quality },
                "required": ["path", "output"],
            },
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::{ColorProfile, Raster};

    /// Send `requests`, one a line, and parse each line of the replies.
    fn exchange(requests: &[Value]) -> Vec<Value> {
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let mut server = Server {
            app: App::headless(FilterSettings::default(), FilterSettings::default()),
            ctx: egui::Context::default(),
        };
        input.lines().filter_map(|line| server.handle(line)).collect()
    }

    fn request(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn call(id: u64, name: &str, arguments: Value) -> Value {
        request(id, "tools/call", json!({ "name": name, "arguments": arguments }))
    }

    /// A small grey TIFF in its own folder.
    fn tiff(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omapix-mcp-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = Raster::new(64, 48, vec![[30000, 32000, 34000, 65535]; 64 * 48]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let path = dir.join("in.tif");
        export::tiff(&doc, &path).unwrap();
        path
    }

    #[test]
    fn initializes_lists_tools_and_rejects_unknown_methods() {
        let replies = exchange(&[
            request(
                1,
                "initialize",
                json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "test" } }),
            ),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            request(2, "tools/list", json!({})),
            request(3, "ping", json!({})),
            request(4, "resources/list", json!({})),
            call(5, "paint_a_moustache", json!({})),
        ]);
        // The notification has no reply.
        assert_eq!(replies.len(), 5);
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "omapix");
        let names: Vec<_> = replies[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "inspect_document",
                "apply_frequency_separation",
                "apply_auto_retouch",
                "apply_denoise",
                "add_adjustment_layer",
                "get_settings",
                "update_settings",
                "export_document"
            ]
        );
        assert_eq!(replies[2]["result"], json!({}));
        assert_eq!(replies[3]["error"]["code"], -32601);
        assert_eq!(replies[4]["error"]["code"], -32602);
    }

    #[test]
    fn a_line_that_isnt_json_is_a_parse_error() {
        let mut server = Server {
            app: App::headless(FilterSettings::default(), FilterSettings::default()),
            ctx: egui::Context::default(),
        };
        let reply = server.handle("{not json").unwrap();
        assert_eq!((reply["id"].clone(), reply["error"]["code"].clone()), (Value::Null, json!(-32700)));
    }

    #[test]
    fn inspects_and_exports_a_tiff() {
        let input = tiff("export");
        let dir = input.parent().unwrap().to_path_buf();
        let replies = exchange(&[
            call(1, "inspect_document", json!({ "path": input })),
            call(2, "export_document", json!({ "path": input, "output": dir.join("out.jpg"), "quality": 80 })),
            call(3, "export_document", json!({ "path": input, "output": dir.join("out.ora") })),
            call(4, "inspect_document", json!({ "path": dir.join("missing.tif") })),
            call(5, "export_document", json!({ "path": input, "output": dir.join("out.gif") })),
        ]);
        let info = &replies[0]["result"]["structuredContent"];
        assert_eq!(replies[0]["result"]["isError"], false);
        assert_eq!(
            (info["width"].clone(), info["height"].clone(), info["bits"].clone()),
            (json!(64), json!(48), json!(16))
        );
        assert_eq!(info["layers"].as_array().unwrap().len(), 1);
        assert_eq!(info["layers"][0]["blend"], "Normal");
        assert_eq!(info["layers"][0]["visible"], true);
        assert_eq!(replies[1]["result"]["isError"], false);
        assert_eq!(io::load(&dir.join("out.jpg")).unwrap().width, 64);
        assert_eq!(io::load(&dir.join("out.ora")).unwrap().layers.len(), 1);
        // Failures are the tool's result, not a protocol error.
        assert_eq!(replies[3]["result"]["isError"], true);
        assert!(replies[4]["result"]["content"][0]["text"].as_str().unwrap().contains("can't write"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frequency_separation_writes_its_layers_to_an_ora() {
        let input = tiff("separation");
        let output = input.with_file_name("out.ora");
        let replies = exchange(&[call(1, "apply_frequency_separation", json!({ "path": input, "output": output, "radius": 4 }))]);
        assert_eq!(replies[0]["result"]["isError"], false, "{}", replies[0]);
        let doc = io::load(&output).unwrap();
        let names: Vec<_> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Background",
                "Low - color/tone",
                "Low Edit",
                "High - texture",
                "High Edit",
                "Frequency Separation"
            ]
        );
        let layers = &replies[0]["result"]["structuredContent"]["layers"];
        assert_eq!(
            (layers[0]["name"].clone(), layers[0]["kind"].clone()),
            (json!("Frequency Separation"), json!("group"))
        );
        std::fs::remove_dir_all(input.parent().unwrap()).ok();
    }

    #[test]
    fn adds_an_adjustment_layer() {
        let input = tiff("adjustment");
        let output = input.with_file_name("out.ora");
        let replies = exchange(&[
            call(1, "add_adjustment_layer", json!({ "path": input, "output": output, "kind": "Curves" })),
            call(2, "add_adjustment_layer", json!({ "path": input, "output": output, "kind": "Sepia" })),
        ]);
        assert_eq!(replies[0]["result"]["isError"], false, "{}", replies[0]);
        let doc = io::load(&output).unwrap();
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.layers[1].adjustment.as_ref().map(|a| a.name()), Some("Curves"));
        assert_eq!(replies[1]["result"]["isError"], true);
        std::fs::remove_dir_all(input.parent().unwrap()).ok();
    }

    /// Denoise on `OMAPIX_DENOISE_PHOTO`. Needs the model
    /// (scripts/fetch-models.sh).
    #[test]
    #[ignore]
    fn denoise_adds_a_denoise_layer() {
        let Ok(photo) = std::env::var("OMAPIX_DENOISE_PHOTO") else { return };
        let output = std::env::temp_dir().join(format!("omapix-mcp-denoise-{}.ora", std::process::id()));
        let replies = exchange(&[call(1, "apply_denoise", json!({ "path": photo, "output": output, "luminance": 60 }))]);
        assert_eq!(replies[0]["result"]["isError"], false, "{}", replies[0]);
        let names: Vec<_> = io::load(&output).unwrap().layers.into_iter().map(|l| l.name).collect();
        assert_eq!(names.last().map(String::as_str), Some("Denoise"), "{}", replies[0]);
        std::fs::remove_file(&output).ok();
    }

    /// Auto Retouch on `OMAPIX_FACE_PHOTO`. Needs the models
    /// (scripts/fetch-models.sh).
    #[test]
    #[ignore]
    fn auto_retouch_builds_a_group_for_each_face() {
        let Ok(photo) = std::env::var("OMAPIX_FACE_PHOTO") else { return };
        let output = std::env::temp_dir().join(format!("omapix-mcp-retouch-{}.ora", std::process::id()));
        let replies = exchange(&[call(1, "apply_auto_retouch", json!({ "path": photo, "output": output, "preset": "Natural" }))]);
        assert_eq!(replies[0]["result"]["isError"], false, "{}", replies[0]);
        let names: Vec<_> = io::load(&output).unwrap().layers.into_iter().map(|l| l.name).collect();
        assert_eq!(names.last().map(String::as_str), Some("Retouch"));
        std::fs::remove_file(&output).ok();
    }
}
