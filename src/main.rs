use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Seek, SeekFrom};
use std::path::Path;
use std::sync::mpsc::channel;

use iced::futures::channel::mpsc::Sender;
use iced::futures::SinkExt;
use iced::widget::{center, Container};
use iced::{subscription, window, Element, Length, Task};
use iced::{
    widget::{column, text, Column},
    Subscription,
};
use notify::event::{MetadataKind, ModifyKind};
use notify::{EventKind, RecursiveMode, Watcher};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

const MAX_FILE_SIZE: u64 = (1024 * 1024 * 1024) * 50;

pub mod logfile;

use logfile::{LogFile, LogFileMessage};

#[derive(Debug, Clone)]
pub enum Message {
    LogFileMessage(String, logfile::LogFileMessage),
    ViewportResized { width: u32, height: u32 },
    None,
}

#[derive(Debug, Clone, Default)]
struct LogTool {
    current_file: Option<String>,
    log_files: HashMap<String, LogFile>,
}

impl LogTool {
    fn update(&mut self, message: Message) -> Task<Message> {
        if self.current_file.is_none() {
            self.current_file = Some(String::from("log.txt"));
        }

        match message {
            Message::LogFileMessage(filename, logfile_message) => {
                println!("Got data to update from!");
                if !self.log_files.contains_key(&filename) {
                    self.log_files.insert(
                        filename.clone(),
                        LogFile::new(filename.clone(), Vec::new(), 32.0),
                    );
                }

                if let Some(file) = self.log_files.get_mut(&filename) {
                    file.update(logfile_message)
                        .map(move |m| Message::LogFileMessage(filename.clone(), m))
                } else {
                    println!("Unable to update file {filename}, file not in list of files.");
                    Task::none()
                }
            }
            Message::ViewportResized { width, height } => {
                if let Some(current_file) = self.current_file.as_ref() {
                    let filename = current_file.to_owned();

                    if let Some(file) = self.log_files.get_mut(current_file) {
                        file.update(LogFileMessage::ViewportResized { width, height })
                            .map(move |m| Message::LogFileMessage(filename.clone(), m))
                    } else {
                        Task::none()
                    }
                } else {
                    Task::none()
                }
            }
            Message::None => Task::none(),
        }
    }

    fn view(&self) -> Column<Message> {
        let counter = text("test");

        let logsview: Element<_> = if let Some(current_filename) = self.current_file.as_ref() {
            let filename = current_filename.clone();

            if let Some(current_file) = self.log_files.get(current_filename) {
                println!("Attempt to view current_file");

                Container::new(
                    current_file
                        .view()
                        .map(move |m| Message::LogFileMessage(filename.clone(), m)),
                )
                .height(Length::Shrink)
                .into()

                /*
                scrollable(
                        column(current_file.lines.iter().map(text).map(Element::from))
                        .spacing(10),
                    )
                    .id(Id::new(format!("scrollable_{current_filename}")))
                    .height(Length::Fill)
                    .into()
                */
            } else {
                center(text("No such file found.")).into()
            }
        } else {
            center(text("No file opened.")).into()
        };

        let interface = column![counter, logsview];
        interface
    }

    fn subscription(&self) -> Subscription<Message> {
        subscription::Subscription::batch(vec![
            window::events().map(|(_id, e)| match e {
                window::Event::Resized { width, height } => {
                    Message::ViewportResized { width, height }
                }
                _ => Message::None,
            }),
            create_logfile_subscription(String::from("log.txt")).map(|m| m),
        ])
    }
}

fn init_reader(file_path: &Path) -> Result<BufReader<File>, Error> {
    let file = File::open(file_path)?;

    let mut reader = BufReader::new(file);

    let meta = std::fs::metadata(file_path)?;

    if meta.len() > MAX_FILE_SIZE {
        let _ = reader.seek(SeekFrom::End(-(MAX_FILE_SIZE as i64)))?;
        // TODO: debug!("File 2 big, only reading last MAX_FILE_SIZE bytes");
        let mut l = Vec::new();
        let _ = reader.read_until(b'\n', &mut l)?;
        // TODO: debug!("Skipping until next new line.");
    }

    Ok(reader)
}

fn read_data_from_file(reader: &mut BufReader<File>) -> Result<Vec<String>, Error> {
    let mut read_data = Vec::new();
    loop {
        let mut l = String::new();
        let bytes_read = reader.read_line(&mut l)?;

        if bytes_read == 0 {
            break;
        }

        read_data.push(l);
    }

    Ok(read_data)
}

fn create_logfile_subscription(file_path: String) -> Subscription<Message> {
    let log_file = LogFile::new(file_path.clone(), Vec::new(), 32.0);

    let stream = log_file.create_receiver();

    iced::subscription::run_with_id(std::any::TypeId::of::<LogFile>(), stream)
}

async fn reader(file_path: &Path, mut output: Sender<Message>) -> Result<(), Error> {
    //let file_path = Path::new("log.txt");
    let filename = file_path.to_string_lossy();
    // TODO: Verify that file exists

    println!("Reading from {filename}");

    if let Err(e) = std::fs::metadata(&file_path) {
        match e.kind() {
            ErrorKind::NotFound => {
                // TODO: Have a look anyway?
                return Err("Unable to find the specified file.".into());
            }
            _ => (),
        }
    }

    let mut reader = init_reader(&file_path)?;

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

    let preexisting_data = read_data_from_file(&mut reader)?;

    if !preexisting_data.is_empty() {
        output
            .send(Message::LogFileMessage(
                filename.to_string(),
                LogFileMessage::NewData(preexisting_data),
            ))
            .await?;
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
                reader = init_reader(&file_path)?;
            }
            EventKind::Modify(kind) => {
                match kind {
                    ModifyKind::Data(_) => {
                        let data = read_data_from_file(&mut reader)?;

                        if !data.is_empty() {
                            println!("Sending data to stream");
                            output
                                .send(Message::LogFileMessage(
                                    filename.to_string(),
                                    LogFileMessage::NewData(data),
                                ))
                                .await?;
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

/*

fn app_view() -> impl IntoView {
    let lines: im::Vector<String> = im::Vector::new();
    let (lines, set_lines) = create_signal(lines);

    std::thread::spawn(move || {
        if let Err(e) = reader(set_lines) {
            // TODO: Error handling.
            panic!("{e:?}");
        }
    });

    container(
        scroll(
            virtual_list(
                VirtualDirection::Vertical,
                VirtualItemSize::Fixed(Box::new(|| 32.0)),
                move || lines.get().enumerate(),
                move |(_, item)| item.clone(),
                move |(index, item)| {
                    text(format!("{index}: {item}"))
                }
            ).style(move |s| s.flex_col().flex_grow(1.0))
        ).style(move |s| s.width_full().height_full().border(1.0))
    ).style(|s| {
        s.size(100i32.pct(), 100i32.pct())
            .padding_vert(20.0)
            .padding_horiz(10.0)
            .flex_col()
            .items_center()
    })
}

*/

fn main() -> iced::Result {
    //floem::launch(app_view);

    iced::program("Application", LogTool::update, LogTool::view)
        .subscription(LogTool::subscription)
        .run()
}
