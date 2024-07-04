use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::ffi::OsStr;
//use std::fs::File;
//use std::io::{BufRead, ErrorKind, Seek, SeekFrom};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use egui::{Label, ScrollArea, TextBuffer, TextEdit, Widget};
use egui_extras::{TableBuilder, Column};

use notify::event::{MetadataKind, ModifyKind};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use crate::{MAX_FILE_SIZE, Error};

use tokio::io::{BufReader, SeekFrom, AsyncSeekExt, AsyncBufReadExt, ErrorKind};
use tokio::fs::File;

use log::{error, debug};

#[derive(Serialize, Deserialize)]
pub struct LogFile {
    pub filename: String,
    pub path: PathBuf,
    pub filter: String,
    #[serde(skip)]
    pub lines: Vec<String>,
    #[serde(skip)]
    receiver: Option<Receiver<Vec<String>>>,
    item_height: f32,
}

impl LogFile {
    // TODO: Change receiver type to Result<Vec<String>, ReadError>?
    pub fn create_receiver(&self, ctx: egui::Context) -> Receiver<Vec<String>> {
        let (sender, receiver) = channel();
        let file_path = self.path.clone();

        tokio::spawn(async move {
            if let Err(e) = reader(file_path.as_path(), sender, ctx).await {
                // TODO: Actual error handling
                error!("Unable to do things with logfile: {e:?}, {:?}", e.source());
            }
        });

        receiver
    }

    pub fn new(path: PathBuf, items: Vec<String>, item_height: f32) -> Self {
        Self {
            filename: path.to_string_lossy().to_string(), 
            path,
            filter: String::new(),
            lines: items,
            receiver: None,
            item_height
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // TODO: Read channel and push data
        if let Some(receiver) = &self.receiver {
            loop {
                let res = receiver.try_recv();

                match res {
                    Ok(v) => {
                        for l in v {
                            self.lines.push(l);
                        }
                    },
                    Err(e) => {
                        match e {
                            TryRecvError::Empty => (),
                            TryRecvError::Disconnected => {
                                self.receiver = None;
                                self.lines.clear();
                            }
                        };

                        break;
                    },
                }
            }
        } else {
            self.receiver = Some(self.create_receiver(ui.ctx().clone()));
        }

        /*
        let filtered = if self.filter.is_empty() {
            self.lines
        } else {
            let f = self.filter.as_str();
            self.lines.iter().filter(|l| l.contains(f)).map(String::to_owned).collect::<Vec<String>>()
        };
        */
        let filtered = self.lines.as_slice();

        // TODO: Table or roll our own inside ScrollArea?
        if false {
            const SCROLLBAR_HEIGHT: f32 = 16.0;

            ScrollArea::horizontal().auto_shrink([false, true])
                .max_height(ui.available_height() - SCROLLBAR_HEIGHT)
                .show(ui, |ui| {
                    let height = ui.available_height();
                    TableBuilder::new(ui)
                        .stick_to_bottom(true)
                        .striped(true)
                        .column(Column::remainder())
                        .auto_shrink([false, false])
                        .max_scroll_height(height - SCROLLBAR_HEIGHT)
                        .body(|body| {
                            body.rows(self.item_height, filtered.len(), |mut row| {
                                let row_index = row.index();
                                row.col(|ui| {
                                    Label::new(self.lines.get(row_index).unwrap_or(&String::from("")))
                                        .wrap_mode(egui::TextWrapMode::Extend)
                                        .ui(ui);
                                    });
                            })
                        });
                });
        } else {
            ui.vertical(|ui| {
                ScrollArea::both()
                    .auto_shrink([false, true])
                    .stick_to_bottom(true)
                    .max_height(ui.available_height() - 32.0)
                    .show_rows(ui, self.item_height, filtered.len(), |ui, row_range| {
                        for row_index in row_range {
                            Label::new(self.lines.get(row_index).unwrap_or(&String::from("")))
                                .wrap_mode(egui::TextWrapMode::Extend)
                                .ui(ui);
                            }
                    });
            });
        }

        ui.horizontal(|ui| {
            ui.horizontal(|ui| {
                ui.label("Filter");
                TextEdit::singleline(&mut self.filter).show(ui);
            });
            ui.horizontal(|ui| {
                ui.label("Highlight");
                TextEdit::singleline(&mut String::new()).show(ui);
            })
        });
    }
}

impl Debug for LogFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("LogFile {}", self.filename))
    }
}

async fn init_reader(file_path: &Path) -> Result<BufReader<File>, Error> {
    let file = File::open(file_path).await?;

    let mut reader = BufReader::new(file);

    let meta = tokio::fs::metadata(file_path).await?;
    //let meta = std::fs::metadata(file_path)?;

    if meta.len() > MAX_FILE_SIZE {
        let _ = reader.seek(SeekFrom::End(-(MAX_FILE_SIZE as i64))).await?;
        // TODO: debug!("File 2 big, only reading last MAX_FILE_SIZE bytes");
        let mut l = Vec::new();
        let _ = reader.read_until(b'\n', &mut l).await?;
        // TODO: debug!("Skipping until next new line.");
    }

    Ok(reader)
}

async fn read_data_from_file(reader: &mut BufReader<File>) -> Result<Vec<String>, Error> {
    let mut read_data = Vec::new();
    loop {
        let mut l = String::new();
        let bytes_read = reader.read_line(&mut l).await?;

        if bytes_read == 0 {
            break;
        }

        read_data.push(l);
    }

    Ok(read_data)
}

async fn reader(file_path: &Path, output: Sender<Vec<String>>, ctx: egui::Context) -> Result<(), Error> {
    //let file_path = Path::new("log.txt");
    let filename = file_path.to_string_lossy();
    // TODO: Verify that file exists

    debug!("Reading from {filename}");

    if let Err(e) = std::fs::metadata(&file_path) {
        match e.kind() {
            ErrorKind::NotFound => {
                // TODO: Have a look anyway?
                return Err("Unable to find the specified file.".into());
            }
            _ => (),
        }
    }

    let mut reader = init_reader(&file_path).await?;

    // TODO: Implement way to choose between recommended and poll?

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        match res {
            Ok(event) => {
                match tx.send(event) {
                    Ok(_) => (),
                    Err(e) => panic!("Unable to send event: {e:?}"),
                };
            }
            Err(e) => panic!("Unable to watch file: {e:?}"),
        };
    })?;

    watcher.watch(
        file_path.to_path_buf().parent().unwrap_or(Path::new(".")),
        RecursiveMode::NonRecursive,
    )?;

    let preexisting_data = read_data_from_file(&mut reader).await?;

    if !preexisting_data.is_empty() {
        output
            .send(preexisting_data)?;
    }

    while let Ok(evt) = rx.recv() {
        if evt
            .paths
            .iter()
            .filter_map(|p| p.file_name())
            .filter(|s| s == &file_path.file_name().unwrap_or(OsStr::new("")))
            .collect::<Vec<_>>()
            .is_empty()
        {
            continue;
        }

        match evt.kind {
            EventKind::Create(_) => {
                reader = init_reader(&file_path).await?;
            }
            EventKind::Modify(kind) => {
                match kind {
                    ModifyKind::Data(_) => {
                        let data = read_data_from_file(&mut reader).await?;

                        if !data.is_empty() {
                            output
                                .send(data)?;
                            ctx.request_repaint();
                        }
                    }
                    ModifyKind::Metadata(k) => {
                        if k == MetadataKind::Any {
                            // When watching a file directly, these event can mean that a file has
                            // been deleted.
                        }
                    }
                    _ => (),
                }
            }
            _ => (),
        }
    }

    Ok(())
}

