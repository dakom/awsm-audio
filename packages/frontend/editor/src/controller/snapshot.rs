//! The read half of the controller surface: project the live editor state into
//! the pure `awsm-audio-schema` [`Graph`] (the audio truth) plus an
//! [`EditorSnapshot`] that also carries the editor-only view-state (node
//! layout, camera, selection). This is what a future MCP transport reads back.

use awsm_audio_schema::{
    Connection, ConnectionSink, ConnectionSource, Graph, Node, Sample, SampleLibrary,
};

// The struct *definitions* (`NodeLayout`/`EditorSnapshot`/`EditorProject`) now
// live in the shared protocol crate; the `impl EditorController` blocks below
// that *build* them stay here (they reach into controller internals).
pub use awsm_audio_editor_protocol::{EditorProject, EditorSnapshot, NodeLayout};

use super::EditorController;

impl EditorController {
    /// Project the canvas into a pure-audio schema [`Graph`] (no layout).
    /// Boundary nodes become `inlets`/`outlets` (ordered top-to-bottom) and the
    /// connections touching them become `Inlet`/`Outlet` endpoints. Saved/played
    /// graphs (no wire ids — they're editor-session-local); use
    /// [`build_graph`](Self::build_graph) directly for an id-bearing snapshot.
    pub fn to_graph(&self) -> Graph {
        self.build_graph(false)
    }

    /// As [`to_graph`](Self::to_graph), but `with_ids` stamps each wire with its
    /// stable editor [`ConnId`](super::ConnId) — what the live **snapshot** carries
    /// so an agent can `disconnect` a single wire by id. The saved document path
    /// leaves them off (`false`) so portable graphs stay pure `from`/`to` edges.
    pub fn build_graph(&self, with_ids: bool) -> Graph {
        use super::BoundaryPort;
        use awsm_audio_schema::{PortDecl, PortId};
        let lock = self.nodes.lock_ref();

        let nodes = lock
            .iter()
            .filter(|n| n.boundary.is_none())
            .map(|n| {
                let label = n.label.get_cloned();
                Node {
                    id: n.id,
                    label: (!label.is_empty()).then_some(label),
                    kind: n.kind.borrow().clone(),
                }
            })
            .collect();

        // Boundary ports, ordered top-to-bottom so indices are stable + visual.
        let boundary_ports = |which: BoundaryPort| -> Vec<PortDecl> {
            let mut v: Vec<_> = lock.iter().filter(|n| n.boundary == Some(which)).collect();
            v.sort_by(|a, b| {
                a.pos
                    .get()
                    .1
                    .partial_cmp(&b.pos.get().1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            v.into_iter()
                .map(|n| PortDecl {
                    id: PortId::from(n.label.get_cloned()),
                    label: None,
                    // Inlets carry an input's default value; outlets ignore it.
                    default: n.default.get(),
                })
                .collect()
        };
        let inlets = boundary_ports(BoundaryPort::Inlet);
        let outlets = boundary_ports(BoundaryPort::Outlet);

        let connections = self
            .connections
            .lock_ref()
            .iter()
            .map(|c| {
                let from = if c.from.boundary == Some(BoundaryPort::Inlet) {
                    ConnectionSource::Inlet {
                        port: PortId::from(c.from.label.get_cloned()),
                    }
                } else if crate::ports::is_sequencer(&c.from.kind.borrow()) {
                    // A sequencer output is a keyed binding, not an audio output.
                    ConnectionSource::SeqOut {
                        node: c.from.id,
                        key: crate::ports::seq_key_at(
                            &c.from.kind.borrow(),
                            c.from_output as usize,
                        )
                        .unwrap_or_default(),
                    }
                } else {
                    ConnectionSource::NodeOutput {
                        node: c.from.id,
                        output: c.from_output,
                    }
                };
                let to = if c.to.boundary == Some(BoundaryPort::Outlet) {
                    ConnectionSink::Outlet {
                        port: PortId::from(c.to.label.get_cloned()),
                    }
                } else {
                    match &c.sink {
                        super::ConnSink::Input(input) => ConnectionSink::NodeInput {
                            node: c.to.id,
                            input: *input,
                        },
                        super::ConnSink::Param(param) => ConnectionSink::NodeParam {
                            node: c.to.id,
                            param: param.clone(),
                        },
                        super::ConnSink::Trigger => ConnectionSink::Trigger { node: c.to.id },
                    }
                };
                Connection {
                    id: with_ids.then_some(c.id),
                    from,
                    to,
                }
            })
            .collect();

        Graph {
            nodes,
            connections,
            inlets,
            outlets,
        }
    }

    /// Project the canvas into a saveable [`SampleLibrary`] document — the
    /// portable, player-consumable form, with every referenced asset (WASM
    /// modules + audio buffers) embedded so it's self-contained.
    pub fn to_library(&self) -> SampleLibrary {
        use awsm_audio_schema::NodeKind;
        // Fold the live canvas back into the active sample, then export them all.
        self.commit_active();
        let samples: Vec<Sample> = self
            .samples
            .borrow()
            .iter()
            .map(|s| s.sample.clone())
            .collect();
        let mut lib = SampleLibrary {
            root: Some(*self.root.borrow()),
            samples,
            listener: Some(self.listener.borrow().clone()),
            ..Default::default()
        };
        // Embed referenced assets across every sample (compiled/decoded copies
        // live in the player; their serializable source in the registries).
        let wasm = self.wasm_assets.borrow();
        let buffers = self.buffer_assets.borrow();
        for sample in &lib.samples.clone() {
            // A bounced Sound's rendered audio buffer (referenced by arrangement
            // clips, not by a node) must be embedded so it survives save/load.
            if let Some(b) = &sample.bounce {
                embed_buffer(&mut lib, &buffers, Some(b.asset));
            }
            for node in &sample.graph.nodes {
                match &node.kind {
                    NodeKind::AudioWorklet(w) => {
                        if let Some(a) = w.module.and_then(|id| wasm.get(&id)) {
                            if !lib.assets.wasm_modules.iter().any(|x| x.id == a.id) {
                                lib.assets.wasm_modules.push(a.clone());
                            }
                        }
                    }
                    NodeKind::AudioBufferSource(b) => embed_buffer(&mut lib, &buffers, b.buffer),
                    NodeKind::Convolver(c) => embed_buffer(&mut lib, &buffers, c.buffer),
                    _ => {}
                }
            }
        }
        lib
    }

    /// Project the project into a full editor [`EditorProject`] (library + a flat
    /// per-node layout across all samples + camera) — what Save writes.
    pub fn to_project(&self) -> EditorProject {
        let library = self.to_library(); // commits the active sample first
        let layout: Vec<NodeLayout> = self
            .samples
            .borrow()
            .iter()
            .flat_map(|s| {
                s.layout
                    .iter()
                    .map(|(id, (x, y))| NodeLayout {
                        id: *id,
                        x: *x,
                        y: *y,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let (pan_x, pan_y) = self.pan.get();
        EditorProject {
            library,
            layout,
            pan_x,
            pan_y,
            zoom: self.zoom.get(),
        }
    }

    /// Build the directory form of the project: an [`EditorProject`] whose every
    /// embedded asset's `source` is rewritten to a project-relative
    /// `assets/<id>.<ext>` path, paired with the `(path, bytes)` files to write
    /// alongside `project.toml`. Imported audio keeps its original bytes/extension,
    /// bounced Sounds become `.wav` (via [`crate::util::encode_wav`]), and WASM
    /// modules become `.wasm`. `Url`-sourced assets stay as URLs (no file).
    fn project_dir_files(&self) -> (EditorProject, Vec<(String, Vec<u8>)>) {
        use awsm_audio_schema::{AudioSource, WasmSource};
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;

        let mut project = self.to_project(); // commits + embeds all referenced assets
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();

        for asset in &mut project.library.assets.buffers {
            let (bytes, ext) = match &asset.source {
                AudioSource::Encoded(data) => match b64.decode(data) {
                    Ok(bytes) => (bytes, ext_from_label(&asset.label, "bin")),
                    Err(_) => continue,
                },
                AudioSource::Pcm {
                    sample_rate,
                    channels,
                } => (
                    crate::util::encode_wav(channels, *sample_rate as u32),
                    "wav".to_string(),
                ),
                // Already a path, or an external URL — leave the source untouched.
                AudioSource::Path(_) | AudioSource::Url(_) => continue,
            };
            let path = format!("assets/{}.{}", asset.id, ext);
            asset.source = AudioSource::Path(path.clone());
            files.push((path, bytes));
        }

        for asset in &mut project.library.assets.wasm_modules {
            let bytes = match &asset.source {
                WasmSource::Base64(data) => match b64.decode(data) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                },
                WasmSource::Path(_) | WasmSource::Url(_) => continue,
            };
            let path = format!("assets/{}.wasm", asset.id);
            asset.source = WasmSource::Path(path.clone());
            files.push((path, bytes));
        }

        (project, files)
    }

    /// Save the project to a picked directory: a root `project.toml` plus the
    /// `assets/` folder of real files. Chromium-only (File System Access API).
    pub async fn save_to_dir(&self, dir: &crate::fs::ProjectDir) -> Result<(), String> {
        let (project, files) = self.project_dir_files();
        let toml = toml::to_string_pretty(&project).map_err(|e| e.to_string())?;
        dir.write_text("project.toml", &toml)
            .await
            .map_err(|e| e.to_string())?;
        for (path, bytes) in files {
            dir.write_bytes(&path, &bytes)
                .await
                .map_err(|e| format!("write {path}: {e}"))?;
        }
        Ok(())
    }

    /// Load a project from a picked directory: read `project.toml`, rehydrate each
    /// `Path`-sourced asset by reading its file back into inline bytes, then apply
    /// it through the normal [`open_project`](Self::open_project) path (which
    /// decodes audio + compiles WASM into the player).
    pub async fn load_from_dir(&self, dir: &crate::fs::ProjectDir) -> Result<(), String> {
        use awsm_audio_schema::{AudioSource, WasmSource};
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;

        let body = dir
            .read_text("project.toml")
            .await
            .map_err(|e| e.to_string())?;
        let mut project: EditorProject =
            toml::from_str(&body).map_err(|e| format!("parse project.toml: {e}"))?;

        for asset in &mut project.library.assets.buffers {
            if let AudioSource::Path(path) = &asset.source {
                let bytes = dir
                    .read_bytes(path)
                    .await
                    .map_err(|e| format!("read {path}: {e}"))?;
                asset.source = AudioSource::Encoded(b64.encode(&bytes));
            }
        }
        for asset in &mut project.library.assets.wasm_modules {
            if let WasmSource::Path(path) = &asset.source {
                let bytes = dir
                    .read_bytes(path)
                    .await
                    .map_err(|e| format!("read {path}: {e}"))?;
                asset.source = WasmSource::Base64(b64.encode(&bytes));
            }
        }

        self.open_project(project);
        Ok(())
    }

    /// Full snapshot including editor-only view-state.
    pub fn snapshot(&self) -> EditorSnapshot {
        let layout = self
            .nodes
            .lock_ref()
            .iter()
            .map(|n| {
                let (x, y) = n.pos.get();
                NodeLayout { id: n.id, x, y }
            })
            .collect();

        let selection = self
            .nodes
            .lock_ref()
            .iter()
            .filter(|n| n.selected.get())
            .map(|n| n.id)
            .collect();

        let (pan_x, pan_y) = self.pan.get();

        EditorSnapshot {
            // Snapshots carry wire ids so an agent can `disconnect` one surgically.
            graph: self.build_graph(true),
            layout,
            pan_x,
            pan_y,
            zoom: self.zoom.get(),
            selection,
            arrangement: self.active_arrangement(),
        }
    }
}

/// Push a referenced buffer asset into `lib` (deduped).
fn embed_buffer(
    lib: &mut SampleLibrary,
    registry: &std::collections::HashMap<
        awsm_audio_schema::AssetId,
        awsm_audio_schema::BufferAsset,
    >,
    id: Option<awsm_audio_schema::AssetId>,
) {
    if let Some(a) = id.and_then(|id| registry.get(&id)) {
        if !lib.assets.buffers.iter().any(|x| x.id == a.id) {
            lib.assets.buffers.push(a.clone());
        }
    }
}

/// A short, lower-cased file extension from an asset's label (e.g. `"Kick.WAV"`
/// → `"wav"`), falling back to `default` when the label has no usable extension.
fn ext_from_label(label: &Option<String>, default: &str) -> String {
    label
        .as_deref()
        .and_then(|l| {
            l.rsplit_once('.').map(|(_, ext)| ext).filter(|ext| {
                !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric())
            })
        })
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_else(|| default.to_string())
}
