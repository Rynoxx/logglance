use std::{collections::VecDeque, fmt::Debug, path::{Path, PathBuf}, sync::mpsc::{channel, Receiver, Sender}};

use log::{debug, error};

use egui::{CentralPanel, TopBottomPanel};
use egui_tiles::{Behavior, Container, SimplificationOptions, Tile, Tiles, Tree, UiResponse};
use serde::{Deserialize, Serialize};

pub const APPLICATION_NAME: &str = "LogTool";
pub const IS_WEB: bool = cfg!(target_arch = "wasm32");

pub type Error = Box<dyn std::error::Error + Send + Sync>;

const MAX_FILE_SIZE: u64 = (1024 * 1024 * 1024) * 50;
const MAX_RECENT_FILES: usize = 20;
const DEFAULT_ROW_SIZE: f32 = 18.0;

pub mod logfile;

use logfile::LogFile;

#[derive(Serialize, Deserialize)]
pub enum TabPane {
    LogFile(LogFile)
}

impl TabPane {
    pub fn ui(&mut self, ui: &mut egui::Ui) -> egui_tiles::UiResponse {
        match self {
            Self::LogFile(f) => f.ui(ui)
        }

        UiResponse::None
    }
}

impl Debug for TabPane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LogFile(v) => v.fmt(f),
        }
    }
}

#[derive(Debug)]
pub enum Message {
    FilesPicked(Vec<PathBuf>)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LogTool {
    tree: Tree<TabPane>,
    recent_files: VecDeque<PathBuf>,
    #[serde(skip)]
    messages: MessageChannel,
    #[serde(skip)]
    behaviour: TabBehaviour,
}

#[derive(Debug)]
pub struct MessageChannel {
    sender: Sender<Message>,
    receiver: Receiver<Message>,
}

impl Default for MessageChannel {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            sender,
            receiver,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TabBehaviour {
}

impl Behavior<TabPane> for TabBehaviour {
    fn tab_title_for_pane(&mut self, pane: &TabPane) -> egui::WidgetText {
        match pane {
            TabPane::LogFile(f) => f.filename.clone().into(),
        }
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile_id: egui_tiles::TileId, pane: &mut TabPane) -> UiResponse {
        pane.ui(ui)
    }

    fn simplification_options(&self) -> SimplificationOptions {
        let mut opts = SimplificationOptions::default();
        opts.all_panes_must_have_tabs = true;
        opts.prune_empty_tabs = true;

        opts
    }

    fn is_tab_closable(&self, _tiles: &Tiles<TabPane>, _tile_id: egui_tiles::TileId) -> bool {
        true
    }
}

impl LogTool {
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // This is also where you can customize the look and feel of egui using `cc.egui_ctx.set_visuals` and `cc.egui_ctx.set_fonts`.

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        if let Some(storage) = cc.storage {
            return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        }

        Default::default()
    }

    fn create_tree() -> egui_tiles::Tree<TabPane> {
        let mut tiles = Tiles::default();
        let tabs = vec![];

        //tabs.push(tiles.insert_pane(TabPane::LogFile(LogFile::new(String::from("log.txt"), vec![], 18.0))));
        //tabs.push(TabPane::LogFile(LogFile::new(String::from("log.txt"), vec![], 18.0)));

        let root = tiles.insert_tab_tile(tabs);

        //Tree::new_tabs("logtool_treepanes", tabs)
        Tree::new("logtool_treepanes", root, tiles)
    }

    pub fn add_tile(&mut self, tab: TabPane) {
        debug!("Add {:?}", tab);
        let id = self.tree.tiles.insert_pane(tab);

        if let Some(root_tile_id) = self.tree.root() {

            // TODO: Use global size for lines?
            match self.tree.tiles.get_mut(root_tile_id) {
                Some(Tile::Container(root_tile)) => {
                    root_tile.add_child(id);
                    debug!("to {:?}", root_tile);

                    match root_tile {
                        Container::Tabs(r) => r.set_active(id),
                        _ => (),
                    }
                },
                Some(Tile::Pane(_)) => (),
                None => (),
            }
        } else {
            self.tree.root = Some(self.tree.tiles.insert_tab_tile(vec![id]));
            debug!("No root!");
        }
    }
}

impl Default for LogTool {
    fn default() -> Self {
        Self {
            tree: Self::create_tree(),
            messages: MessageChannel::default(),
            recent_files: VecDeque::new(),
            behaviour: TabBehaviour { },
        }
    }
}

impl eframe::App for LogTool {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Put your widgets into a `SidePanel`, `TopBottomPanel`, `CentralPanel`, `Window` or `Area`.
        // For inspiration and more examples, go to https://emilk.github.io/egui

        if let Ok(msg) = self.messages.receiver.try_recv() {
            debug!("Got message! {msg:?}");

            match msg {
                Message::FilesPicked(files) => {
                    debug!("{files:?}");
                    for path in files {
                        self.add_tile(TabPane::LogFile(LogFile::new(path.clone(), Vec::new(), DEFAULT_ROW_SIZE)));

                        // TODO: Move from whatever position to front
                        if !self.recent_files.contains(&path) {
                            self.recent_files.push_front(path);
                        } else {
                            let filtered = self.recent_files.iter().filter(|p| p != &&path).map(|p| p.to_owned());
                            self.recent_files = VecDeque::from_iter(filtered);
                            self.recent_files.push_front(path);
                        }

                        if self.recent_files.len() > MAX_RECENT_FILES {
                            self.recent_files.pop_back();
                        }
                    }

                    debug!("{:?}", self.tree.tiles);
                    ctx.request_repaint();
                }
            }
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            // The top panel is often a good place for a menu bar:

            egui::menu::bar(ui, |ui| {
                // NOTE: no File->Quit on web pages!
                if !IS_WEB {
                    ui.menu_button("File", |ui| {
                        // TODO: Add "Open File", maybe even a list of X recent files?

                        if ui.button("Open File").clicked() {
                            let file_sender = self.messages.sender.clone();

                            tokio::spawn(async move {
                                if let Some(files) = rfd::AsyncFileDialog::new().pick_files().await {
                                    if let Err(e) = file_sender.send(Message::FilesPicked(
                                            files
                                            .into_iter()
                                            .map(|f| f.path().to_owned())
                                            .collect::<Vec<PathBuf>>())) {
                                        // TODO: Error handling
                                        error!("Unable to send to message channel: {e:?}")
                                    }
                                }
                            });

                            ui.close_menu();
                        }

                        if self.recent_files.is_empty() {
                            // Extra spaces at end to add padding to ensure it keeps style when
                            // using it as a submenu button.
                            // TODO: Better way to handle this?
                            ui.label("Recent files  ");
                        } else {
                            ui.menu_button("Recent files", |ui| {
                                for file in &self.recent_files {
                                    if ui.button(file.to_string_lossy().to_string()).clicked() {
                                        if let Err(e) = self.messages.sender.send(Message::FilesPicked(vec![file.to_owned()])) {
                                            // TODO: Error handling
                                            error!("Unable to send message to channel: {e:?}");
                                        }

                                        ui.close_menu()
                                    }
                                }
                            });
                        }

                        if ui.button("Quit").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });

                    ui.add_space(16.0);
                }

                egui::widgets::global_dark_light_mode_buttons(ui);
            });
        });

        TopBottomPanel::bottom("bottom_panel").show(ctx, powered_by_egui_and_eframe);

        CentralPanel::default().show(ctx, |ui| {
            self.tree.ui(&mut self.behaviour, ui);
        });
    }
}

fn powered_by_egui_and_eframe(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.label("Powered by ");
        ui.hyperlink_to("egui", "https://github.com/emilk/egui");
        ui.label(" and ");
        ui.hyperlink_to(
            "eframe",
            "https://github.com/emilk/egui/tree/master/crates/eframe",
        );
        ui.label(".");
    });
}
