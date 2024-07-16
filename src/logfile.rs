use encoding_rs::Encoding;
use rayon::prelude::*;

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::Instant;

use eframe::egui::{
    self, text::LayoutJob, Color32, Label, ScrollArea, TextFormat, TextStyle, Vec2, Widget,
};

use crate::Error;
use egui_extras::{Size, StripBuilder};
use notify::event::{MetadataKind, ModifyKind};
use notify::{EventKind, RecursiveMode, Watcher};
use rayon::iter::IntoParallelRefIterator;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom};

use log::{debug, error};

const SPACING_FOR_SCROLLBAR: f32 = 8.0;

// TODO: Is there a way to make this dynamic?
static AVAILABLE_ENCODINGS: [&'static Encoding; 34] = [
    encoding_rs::UTF_8,
    encoding_rs::UTF_16BE,
    encoding_rs::UTF_16LE,
    encoding_rs::ISO_8859_2,
    encoding_rs::ISO_8859_3,
    encoding_rs::ISO_8859_4,
    encoding_rs::ISO_8859_5,
    encoding_rs::ISO_8859_6,
    encoding_rs::ISO_8859_7,
    encoding_rs::ISO_8859_8,
    encoding_rs::ISO_8859_10,
    encoding_rs::ISO_8859_13,
    encoding_rs::ISO_8859_14,
    encoding_rs::ISO_8859_15,
    encoding_rs::ISO_8859_16,
    encoding_rs::WINDOWS_874,
    encoding_rs::WINDOWS_1250,
    encoding_rs::WINDOWS_1251,
    encoding_rs::WINDOWS_1252,
    encoding_rs::WINDOWS_1253,
    encoding_rs::WINDOWS_1254,
    encoding_rs::WINDOWS_1255,
    encoding_rs::WINDOWS_1256,
    encoding_rs::WINDOWS_1257,
    encoding_rs::WINDOWS_1258,
    encoding_rs::GBK,
    encoding_rs::BIG5,
    encoding_rs::EUC_JP,
    encoding_rs::EUC_KR,
    encoding_rs::IBM866,
    encoding_rs::GB18030,
    encoding_rs::KOI8_R,
    encoding_rs::KOI8_U,
    encoding_rs::SHIFT_JIS,
];

const MAX_FILE_SIZE: u64 = (2u64.pow(30)) * 4; // 4GiB
const MAX_ROWS: u64 = (10u64.pow(6)) * 120; // 120 million, filtering perfromance and general memory usage
                                            // takes a big hit around here. Better stop before.

pub fn humanreadable_bytes(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::BINARY)
}

pub fn send_err_to_error(e: std::sync::mpsc::SendError<LogFileMessage>) -> crate::Error {
    crate::Error::Other(e.into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    pub full: String,
    pub chunks: Option<Vec<TextChunk>>,
    pub default_format: TextFormat,
}

impl Line {
    pub fn new(txt: String, format: TextFormat) -> Self {
        Self {
            full: txt,
            chunks: None,
            default_format: format,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut layout_job = LayoutJob::default();

        match self.chunks.as_ref() {
            Some(chunks) => {
                for chunk in chunks {
                    layout_job.append(
                        &chunk.text,
                        0.0,
                        chunk.format.clone().unwrap_or(self.default_format.clone()),
                    );
                }
            }
            None => layout_job.append(&self.full, 0.0, self.default_format.clone()),
        }

        Label::new(layout_job).extend().ui(ui);
    }
}

impl From<String> for Line {
    fn from(value: String) -> Self {
        Self::new(value, TextFormat::default())
    }
}

impl From<&str> for Line {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextChunk {
    pub text: String,
    pub format: Option<TextFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Search {
    pub string: String,
    pub is_regex: bool,
    pub case_insensitive: bool,
    #[serde(skip)]
    pub regex: Option<Regex>,
    #[serde(skip)]
    changed: bool,
}

impl Search {
    pub fn is_empty(&self) -> bool {
        self.string.is_empty()
    }

    fn create_regex(&self) -> Result<Regex, regex::Error> {
        let regex_pattern = if self.is_regex {
            &self.string
        } else {
            &regex::escape(&self.string)
        };

        RegexBuilder::new(&regex_pattern)
            .case_insensitive(self.case_insensitive)
            .build()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, additional_content: impl FnOnce(&mut egui::Ui)) {
        self.changed = false;

        let mut data_changed = false;

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("Search text");

                let txt_changed = ui.text_edit_singleline(&mut self.string).changed();
                data_changed = data_changed || txt_changed;
            });

            ui.horizontal(|ui| {
                let regex_checkbox_changed = ui.checkbox(&mut self.is_regex, "Regex?").changed();

                let case_checkbox_changed = ui
                    .checkbox(&mut self.case_insensitive, "Case Insensitive?")
                    .changed();

                data_changed = data_changed || regex_checkbox_changed || case_checkbox_changed;

                additional_content(ui);
            });
        });

        //let data_changed = txt_changed || regex_checkbox_changed || case_checkbox_changed;

        self.changed = (!self.string.is_empty() && self.regex.is_none()) || data_changed;

        // TODO: Ugly to have in UI function, can we move this to a better place?
        if self.changed {
            match self.create_regex() {
                Ok(r) => {
                    self.regex = Some(r);
                }
                Err(e) => {
                    self.regex = None;
                    ui.colored_label(Color32::RED, format!("Invalid regex supplied: {e:?}"));
                }
            }
        }
    }

    pub fn changed(&self) -> bool {
        self.changed
    }
}

// TODO: Change color of the matching text?
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Filter {
    pub search: Search,
    pub filter: bool,
    #[serde(skip)]
    changed: bool,
}

impl Filter {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut checkbox_changed = false;
        self.search.ui(ui, |ui| {
            // TODO: Better label?
            checkbox_changed = ui.checkbox(&mut self.filter, "Filter?").changed();
        });

        // TODO: Buttons to scroll up/down to search results?

        self.changed = checkbox_changed || self.search.changed();
    }

    /// Will return None if there is nothing to filter on
    pub fn filter<'a>(&self, it: &'a Vec<String>) -> Option<Vec<String>> {
        if let Some(r) = self.search.regex.as_ref() {
            Some(
                it.par_iter()
                    .filter(|l| r.is_match(l))
                    .map(String::to_owned)
                    .collect::<Vec<String>>(),
            )
        } else {
            None
        }
    }

    pub fn changed(&self) -> bool {
        self.changed
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RowHighlight {
    pub search: Search,
    pub bg_color: Color32,
    pub fg_color: Color32,
    #[serde(skip)]
    pub(crate) should_delete: bool,
}

impl RowHighlight {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.search.ui(ui, |ui| {
                ui.label("Bg color");
                ui.color_edit_button_srgba(&mut self.bg_color);

                ui.label("Text color");
                ui.color_edit_button_srgba(&mut self.fg_color);
            });

            self.should_delete = ui
                .button("X")
                .on_hover_ui(|ui| {
                    ui.label("Remove row highlight");
                })
                .clicked();
        });
    }
}

impl Default for RowHighlight {
    fn default() -> Self {
        Self {
            bg_color: Color32::DARK_GREEN,
            fg_color: Color32::LIGHT_GREEN,
            search: Search::default(),
            should_delete: false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RowModifier {
    pub filter: Filter,
    pub row_highlights: Vec<RowHighlight>,
}

impl RowModifier {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                StripBuilder::new(ui)
                    .size(Size::relative(0.4))
                    .size(Size::relative(0.59))
                    .horizontal(|mut strip| {
                        strip.cell(|ui| {
                            ui.vertical(|ui| {
                                ui.label("Filter/Search rows");

                                ui.horizontal(|ui| {
                                    self.filter.ui(ui);
                                });
                            });
                        });

                        strip.cell(|ui| {
                            ScrollArea::vertical()
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label("Highlight rows");

                                        if ui
                                            .button("+")
                                            .on_hover_ui(|ui| {
                                                ui.label("Add new row highlight");
                                            })
                                            .clicked()
                                        {
                                            self.row_highlights.push(RowHighlight::default());
                                        }

                                        ui.add_space(4.0);

                                        ui.vertical(|ui| {
                                            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);

                                            let mut highlights_to_remove: Vec<usize> = Vec::new();

                                            for (index, row_highlight) in
                                                self.row_highlights.iter_mut().enumerate()
                                            {
                                                row_highlight.ui(ui);

                                                if row_highlight.should_delete {
                                                    highlights_to_remove.push(index);
                                                }
                                            }

                                            for index in highlights_to_remove {
                                                self.row_highlights.remove(index);
                                            }
                                        });
                                    });

                                    ui.add_space(SPACING_FOR_SCROLLBAR);
                                });
                        });
                    });
            });
    }

    pub fn generate_line(&self, text: &str) -> Line {
        let mut l: Line = text.into();

        for row_highlight in &self.row_highlights {
            if row_highlight.search.is_empty() {
                continue;
            }

            if let Some(re) = row_highlight.search.regex.as_ref() {
                if re.is_match(text) {
                    let mut format = TextFormat::default();
                    format.background = row_highlight.bg_color.clone();
                    format.color = row_highlight.fg_color.clone();

                    l.default_format = format;
                    break;
                }
            }
        }

        if let Some(re) = self.filter.search.regex.as_ref() {
            let mut chunks: Vec<TextChunk> = Vec::new();

            let mut last_end = 0;

            for m in re.find_iter(&text) {
                if m.start() > 0 {
                    chunks.push(TextChunk {
                        text: text[last_end..m.start()].to_string(),
                        format: None,
                    });
                }

                chunks.push(TextChunk {
                    text: m.as_str().to_owned(),
                    format: Some(TextFormat {
                        color: Color32::RED,
                        ..Default::default()
                    }),
                });

                last_end = m.end()
            }

            chunks.push(TextChunk {
                text: text[last_end..].to_string(),
                format: None,
            });

            l.chunks = Some(chunks);
        }

        l
    }
}

#[derive(Debug)]
pub enum LogFileMessage {
    FileData(Vec<String>),
    Error(crate::Error),
    ShowRestrictFileSizeDialog(u64, Sender<bool>),
    RestrictFileSize(bool),
    SetEncoding(Option<&'static Encoding>),
}

#[derive(Clone, Debug, Default)]
pub enum RestrictFileSize {
    #[default]
    Initializing,
    ShowRestrictFileSizeDialog(u64, Sender<bool>),
    RestrictedFileSize,
    UnrestrictedFileSize,
}

// TODO: Some better state management?
#[derive(Serialize, Deserialize)]
pub struct LogFile {
    pub filename: String,
    pub path: PathBuf,
    #[serde(default)]
    pub encoding: Option<&'static Encoding>,
    #[serde(skip, default)]
    pub errors: Vec<crate::Error>,
    #[serde(skip)]
    pub restrict_filesize: RestrictFileSize,
    #[serde(default)]
    pub row_modifier: RowModifier,
    #[serde(skip)]
    pub lines: Vec<String>,
    #[serde(skip)]
    receiver: Option<Receiver<LogFileMessage>>,
    #[serde(skip)]
    sender: Option<Sender<LogFileMessage>>,
    #[serde(skip, default)]
    recalculate_filter_cache: bool,
    #[serde(skip)]
    filter_cache: Option<Vec<String>>,
    #[serde(skip)]
    pub thread: Option<JoinHandle<()>>,
}

impl LogFile {
    pub fn reload_with_encoding(&mut self, encoding: &'static Encoding) {
        self.encoding = Some(encoding);

        if let Some(thread) = self.thread.as_ref() {
            thread.abort();
        }

        self.thread = None;
        self.receiver = None;
    }

    // TODO: Change receiver type to Result<Vec<String>, ReadError>?
    pub fn create_receiver(
        &mut self,
        ctx: egui::Context,
    ) -> (JoinHandle<()>, Receiver<LogFileMessage>) {
        let (sender, receiver) = channel();
        let file_path = self.path.clone();

        self.sender = Some(sender.clone());
        let encoding = self.encoding.clone();

        // TODO: Let users choose encoding.
        let handle = tokio::spawn(async move {
            if let Err(e) = reader(file_path.as_path(), sender, ctx, encoding).await {
                // TODO: Actual error handling
                error!("LogFile reader thread failed: {e:?}");
            }
        });

        (handle, receiver)
    }

    pub fn new(path: PathBuf, items: Vec<String>) -> Self {
        Self {
            filename: path.to_string_lossy().to_string(),
            path,
            row_modifier: RowModifier::default(),
            lines: items,
            restrict_filesize: RestrictFileSize::default(),
            receiver: None,
            sender: None,
            recalculate_filter_cache: false,
            filter_cache: None,
            thread: None,
            encoding: None,
            errors: Vec::new(),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some(receiver) = &self.receiver {
            loop {
                let res = receiver.try_recv();

                match res {
                    Ok(msg) => match msg {
                        LogFileMessage::FileData(v) => {
                            if let Some(cache) = self.filter_cache.as_mut() {
                                if !self.row_modifier.filter.search.is_empty()
                                    && self.row_modifier.filter.filter
                                    && self.row_modifier.filter.search.regex.is_some()
                                {
                                    if let Some(filtered) = self.row_modifier.filter.filter(&v) {
                                        cache.extend(filtered);
                                    } else {
                                        // Unable to incrementally fill the filter cache.
                                        self.recalculate_filter_cache = true;
                                    }
                                }
                            } else {
                                self.recalculate_filter_cache = true;
                            }

                            self.lines.extend(v);
                        },
                        LogFileMessage::ShowRestrictFileSizeDialog(size, sender) => {
                            self.restrict_filesize = RestrictFileSize::ShowRestrictFileSizeDialog(size, sender);
                        },
                        LogFileMessage::RestrictFileSize(response) => {
                            self.restrict_filesize = if response {
                                RestrictFileSize::RestrictedFileSize
                            } else {
                                RestrictFileSize::UnrestrictedFileSize
                            };
                        },
                        LogFileMessage::Error(e) => {
                            error!("Error when handling file: {e:?}");
                            self.errors.push(e);
                        },
                        LogFileMessage::SetEncoding(encoding) => {
                            self.encoding = encoding;
                        },
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
                    }
                }
            }
        } else {
            let (thread, receiver) = self.create_receiver(ui.ctx().clone());
            self.thread = Some(thread);
            self.receiver = Some(receiver);
            self.recalculate_filter_cache = true;
        }

        match self.restrict_filesize.clone() {
            RestrictFileSize::Initializing => (),
            RestrictFileSize::UnrestrictedFileSize => (), // NOOP
            RestrictFileSize::RestrictedFileSize => {
                while self.lines.len() > MAX_ROWS as usize {
                    self.lines.remove(0);
                }
            }
            RestrictFileSize::ShowRestrictFileSizeDialog(size, sender) => {
                egui::Window::new("Large File")
                    .default_open(true)
                    .default_size([384.0, 128.0])
                    .collapsible(false)
                    .show(ui.ctx(), |ui| {
                        // TODO: Show human readable filesize and row number?
                        ui.label(format!(
                            r#"The file you're attempting to open is quite big ({}).
Files larger than {max} require lots of RAM to open due to memory overhead.
Do you want to open this file in restricted mode?

Restricted mode only reads the last {max} and {MAX_ROWS} rows of the file."#,
                            humanreadable_bytes(size),
                            max = humanreadable_bytes(MAX_FILE_SIZE)
                        ));

                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            if ui.button("Open in restricted mode").clicked() {
                                self.restrict_filesize = RestrictFileSize::RestrictedFileSize;

                                if let Err(e) = sender.send(true) {
                                    error!("Unable to send data to file thread: {e:?}");
                                }

                                debug!("Open {} in restricted mode", self.filename);
                            }

                            if ui.button("Open unrestricted").clicked() {
                                self.restrict_filesize = RestrictFileSize::UnrestrictedFileSize;

                                if let Err(e) = sender.send(false) {
                                    error!("Unable to send data to file thread: {e:?}");
                                }

                                debug!("Open {} in unrestricted mode", self.filename);
                            }
                        });
                    });
            }
        }

        if self.recalculate_filter_cache {
            self.filter_cache =
                if self.row_modifier.filter.search.is_empty() || !self.row_modifier.filter.filter {
                    None
                } else {
                    // TODO: self.filter.regex should be some
                    self.row_modifier.filter.filter(&self.lines)
                };

            self.recalculate_filter_cache = false;
        }

        if self.lines.is_empty() {
            ui.vertical_centered_justified(|ui| {
                ui.add_space(50.0);
 
                if self.errors.is_empty() {
                    ui.label("Loading data...");
                    // TODO: Would be neat if we had some sort of byte or percentage counter here?
                    ui.spinner();
                } else {
                    ui.label("ERROR");

                    for err in &self.errors {
                        // TODO: Better way to display errors?
                        ui.label(err.to_string());
                    }
                }
            });
        } else {
            let text_height = ui.text_style_height(&TextStyle::Body);

            let mut clicked_encoding: Option<&'static Encoding> = None;

            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    StripBuilder::new(ui)
                        // TODO: I don't like these magic numbers. Is there a good way to calculate
                        // these hardcoded numbers dynamically?
                        .size(Size::remainder().at_least(text_height * 10.0))
                        .size(Size::exact(text_height * 8.0).at_least(text_height))
                        .size(Size::exact(text_height * 2.0))
                        .vertical(|mut strip| {
                            strip.cell(|ui| {
                                ui.vertical(|ui| {
                                    let filtered = if let Some(f) = self.filter_cache.as_ref() {
                                        f
                                    } else {
                                        self.lines.as_ref()
                                    };

                                    // TODO: Is there a better way than using negative spacing?
                                    ui.spacing_mut().item_spacing = Vec2::new(0.0, -10.0);

                                    ScrollArea::both()
                                        .auto_shrink([false, true])
                                        .stick_to_bottom(true)
                                        //.max_height(ui.available_height() - (text_height * 4.0))
                                        .show_rows(
                                            ui,
                                            text_height,
                                            filtered.len(),
                                            |ui, row_range| {
                                                for row_index in row_range {
                                                    if let Some(line) = filtered.get(row_index) {
                                                        self.row_modifier
                                                            .generate_line(line)
                                                            .ui(ui);
                                                    }
                                                }
                                            },
                                        );
                                });
                            });

                            strip.cell(|ui| {
                                ui.separator();
                                self.row_modifier.ui(ui);
                            });

                            strip.cell(|ui| {
                                ui.separator();
                                ui.horizontal(|ui| {
                                    if let Some(encoding) = self.encoding.as_ref() {
                                        ui.add_space(1.0);

                                        ui.menu_button(format!("Encoding: {}", encoding.name()), |ui| {
                                            for enc in AVAILABLE_ENCODINGS {
                                                if ui.button(enc.name()).clicked() {
                                                    clicked_encoding = Some(enc);
                                                }
                                            }
                                        });
                                    }
                                });
                            });
                        });
                });

            if let Some(enc) = clicked_encoding {
                self.reload_with_encoding(enc);
            }
        }

        // TODO: Wait X miliseconds to await further changes?
        if self.row_modifier.filter.changed() {
            self.recalculate_filter_cache = true;
        }
    }
}

impl Debug for LogFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("LogFile {}", self.filename))
    }
}

async fn init_reader(file_path: &Path, restrict_filesize: bool, encoding: Option<&'static Encoding>) -> Result<(BufReader<File>, &'static Encoding), Error> {
    let file = File::open(file_path).await?;
    let mut reader = BufReader::new(file);

    let encoding = match encoding {
        Some(e) => e,
        None => {
            let max_bytes_to_read = 24 * 1024 * 1024;
            let mut detection_buffer = vec![0; max_bytes_to_read];

            let num_bytes = reader.read(&mut detection_buffer).await?;
            reader.seek(SeekFrom::Start(0)).await?;

            match Encoding::for_bom(&detection_buffer[0 .. num_bytes]) {
                Some((e, num_bom_bytes)) => {
                    debug!("Detected encoding: {}, based on {num_bom_bytes} BOM bytes", e.name());
                    e
                },
                None => {
                    let mut detector = chardetng::EncodingDetector::new();

                    detector.feed(&detection_buffer[0 .. num_bytes ], num_bytes < max_bytes_to_read);
                    // Hard to make it decide between
                    let (e, good_score) = detector.guess_assess(None, true);
                    debug!("Detected encoding: {}, based on {num_bytes} bytes read. Is there likely a better encoding? {good_score}", e.name());
                    e
                }
            }
        }
    };

    let meta = tokio::fs::metadata(file_path).await?;

    debug!(
        "Is file ({}) bigger than max file size ({MAX_FILE_SIZE}): {}",
        meta.len(),
        meta.len() > MAX_FILE_SIZE
    );

    if restrict_filesize && meta.len() > MAX_FILE_SIZE {
        // Additional 512 bytes to increase likelyhood of not skipping too much data. E.g. include
        // potential linebreaks etc
        let seek_to = MAX_FILE_SIZE + 512;
        debug!("File too big, only reading last {seek_to} bytes");
        let _ = reader.seek(SeekFrom::End(-(seek_to as i64))).await?;
        let mut l = Vec::new();
        debug!("Skipping until next new line.");
        let _ = reader.read_until(b'\n', &mut l).await?;
    }

    Ok((reader, encoding))
}

async fn read_data_from_file(
    reader: &mut BufReader<File>,
    restrict_row_number: bool,
    encoding: &'static Encoding,
) -> Result<Vec<String>, Error> {
    let mut read_data = VecDeque::new();

    let mut lines = 0;

    loop {
        let mut buf = Vec::new();
        let bytes_read = reader.read_until(b'\n', &mut buf).await?;

        if bytes_read == 0 {
            break;
        }

        let (output, _encoding, _contains_invalid_content) = encoding.decode(buf.as_slice());//encoding_rs::UTF_8.decode(buf.as_slice());

        lines += 1;

        if lines % 100000 == 0 {
            debug!("{lines} lines read. Vec capacity: {}", read_data.capacity());
        }

        if restrict_row_number && lines > MAX_ROWS {
            read_data.pop_front();
        }

        read_data.push_back(output.to_string());
        //read_data.push_back(String::from_utf8(buf)?)
    }

    read_data.shrink_to_fit();

    Ok(read_data.into())
}

async fn reader(
    file_path: &Path,
    output: Sender<LogFileMessage>,
    ctx: egui::Context,
    encoding: Option<&'static Encoding>,
) -> Result<(), Error> {
    let filename = file_path.to_string_lossy();
    debug!("Opening {filename}");

    let file_meta = match tokio::fs::metadata(&file_path).await {
        Ok(meta) => {
            debug!("File {file_path:?} exists.");
            debug!(
                "File is {} bytes large. Preallocate {} lines?",
                meta.len(),
                meta.len().saturating_div(128)
            );
            meta
        },
        Err(e) => {
            let msg = format!("Unable to open the specified file: {e:?}");
            output.send(LogFileMessage::Error(e.into())).map_err(send_err_to_error)?;
            return Err(msg.into());
        }
    };

    let restrict_filesize = if file_meta.len() > MAX_FILE_SIZE {
        debug!("File big ({}), open window.", file_meta.len());
        let (tx, rx) = channel();
        output.send(LogFileMessage::ShowRestrictFileSizeDialog(
            file_meta.len(),
            tx,
        )).map_err(send_err_to_error)?;
        ctx.request_repaint();

        rx.recv()?
    } else {
        output.send(LogFileMessage::RestrictFileSize(true)).map_err(send_err_to_error)?;

        true
    };

    let start = Instant::now();
    debug!("Reading from {filename}");

    let (mut reader, mut encoding) = init_reader(&file_path, restrict_filesize, encoding).await?;

    output.send(LogFileMessage::SetEncoding(Some(encoding))).map_err(send_err_to_error)?;
    // TODO: Implement way to choose between recommended and poll? E.g. in case of file paths that
    // don't quite support inotify etc.

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

    debug!("Read initial data from file");
    //let preexisting_data =
    match read_data_from_file(&mut reader, restrict_filesize, encoding).await {
        Ok(preexisting_data) => {
            if !preexisting_data.is_empty() {
                output.send(LogFileMessage::FileData(preexisting_data)).map_err(send_err_to_error)?;
                ctx.request_repaint();
            }
        },
        Err(e) => {
            output.send(LogFileMessage::Error(e)).map_err(send_err_to_error)?;
            ctx.request_repaint();
        }
    }

    debug!("Took {:?} to create reader and read existing data", Instant::now().duration_since(start));

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
                (reader, encoding) = init_reader(&file_path, restrict_filesize, Some(encoding)).await?;
            }
            EventKind::Modify(kind) => {
                match kind {
                    ModifyKind::Data(_) => {
                        match read_data_from_file(&mut reader, restrict_filesize, encoding).await {
                            Ok(data) => {
                                if !data.is_empty() {
                                    output.send(LogFileMessage::FileData(data)).map_err(send_err_to_error)?;
                                    ctx.request_repaint();
                                }
                            },
                            Err(e) => {
                                output.send(LogFileMessage::Error(e)).map_err(send_err_to_error)?;
                                ctx.request_repaint();
                            }
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

