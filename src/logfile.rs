use std::path::Path;

use iced::{
    alignment::Vertical,
    futures::channel::mpsc::{channel as iced_channel, Receiver},
    widget::{
        container, scrollable::{self, AbsoluteOffset, Id, Properties, RelativeOffset}, text::LineHeight, Column, Container, Row, Scrollable, Text
    },
    Background, Color, Element, Length, Padding, Pixels, Task,
};

use crate::{reader, Message};

const ADDITIONAL_OFFSET: f32 = 2.0;

#[derive(Debug, Clone)]
pub struct LogFile {
    pub id: Id,
    pub filename: String,
    pub lines: Vec<String>,
    visible_start: usize,
    visible_end: usize,
    item_height: f32,
    offset: f32,
    items_per_page: usize,
}

#[derive(Debug, Clone)]
pub enum LogFileMessage {
    ScrollChanged(scrollable::Viewport),
    ViewportResized { width: u32, height: u32 },
    SetItemHeight(f32),
    NewData(Vec<String>),
}

impl LogFile {
    pub fn create_receiver(&self) -> Receiver<Message> {
        let (sender, receiver) = iced_channel(100);
        let file_path = self.filename.clone();

        let _ = smol::spawn(async move {
            if let Err(e) = reader(Path::new(&file_path.clone()), sender).await {
                // TODO: Actual error handling
                eprintln!("Unable to do things with logfile: {e:?}, {:?}", e.source());
            }
        })
        .detach();

        receiver
    }
}

impl LogFile {
    pub fn new(filename: String, items: Vec<String>, item_height: f32) -> Self {
        Self {
            id: Id::new(format!("logfile_{}_{:?}", filename, Id::unique())),
            filename,
            lines: items,
            visible_start: 0,
            visible_end: 0,
            item_height,
            offset: 0.0,
            items_per_page: 100,
        }
    }

    pub fn scroll(&self) -> Task<LogFileMessage> {
        scrollable::scroll_to(self.id.clone(), AbsoluteOffset {
            ..Default::default()
        })
    }

    pub fn snap_to_end(&mut self) -> Task<LogFileMessage> {
        if self.items_per_page > 0 && !self.lines.is_empty() {
            self.visible_end = self.lines.len();
            self.visible_start = self.visible_end.saturating_sub(self.items_per_page);
        }

        scrollable::snap_to(self.id.clone(), RelativeOffset { x: 0.0, y: 1.0 })
    }

    pub fn update(&mut self, message: LogFileMessage) -> Task<LogFileMessage> {
        match message {
            LogFileMessage::ScrollChanged(viewport) => {
                println!("Recalculating.");

                let bounds = viewport.bounds();

                self.items_per_page = ((bounds.height / self.item_height) + ADDITIONAL_OFFSET).ceil() as usize;

                self.offset = viewport.absolute_offset().y;
                self.visible_start = (self.offset / self.item_height) as usize;

                self.visible_end = self.visible_start.saturating_add(self.items_per_page);

                if self.visible_end > self.lines.len() {
                    self.visible_end = self.lines.len();
                    self.visible_start = self.visible_end.saturating_sub(self.items_per_page);
                }

                Task::none()
            },
            LogFileMessage::NewData(data) => {
                for line in data {
                    self.lines.push(line);
                }

                // TODO: If not set to follow, don't snap.
                self.snap_to_end()
            },
            LogFileMessage::ViewportResized {
                width: _,
                height: _,
            } => {
                // TODO: Trigger something so that we can recalculate viewport.
                //scrollable::snap_to(self.id.clone(), RelativeOffset { x: 0.0, y: 1.0 })
                // TODO: If set to follow:
                self.snap_to_end()
            },
            LogFileMessage::SetItemHeight(new_height) => {
                self.item_height = new_height;

                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<LogFileMessage> {
        let vis_end = if self.visible_end == 0 && !self.lines.is_empty() {
            50
        } else {
            self.visible_end
        };

        let style_black = container::Style::default()
            .with_background(Background::Color(Color::new(0.3, 0.3, 0.3, 0.2)));
        let style_white = container::Style::default()
            .with_background(Background::Color(Color::new(1.0, 1.0, 1.0, 0.0)));

        println!("first {}, last {}", self.visible_start, vis_end);
        let visible_items: Vec<Element<_>> = self.lines[self.visible_start..vis_end]
            .iter()
            .enumerate()
            .map(move |(idx, item)| {
                let index = idx.clone();

                Container::new(
                    Text::new(item.clone())
                        .size(self.item_height * 0.7)
                        .line_height(LineHeight::Absolute(Pixels(self.item_height)))
                        .vertical_alignment(Vertical::Center)
                        .height(Length::Fixed(self.item_height)),
                )
                .style(move |_| {
                    if index % 2 == 0 {
                        style_black.clone()
                    } else {
                        style_white.clone()
                    }
                })
                .padding(Padding {
                    top: 0.0,
                    left: self.item_height * 0.5,
                    bottom: 0.0,
                    right: self.item_height * 0.5
                })
                .into()
            })
            .collect();

        let content = Column::with_children(visible_items)
            .height(Length::Fixed((self.lines.len() as f32) * self.item_height))
            .padding(Padding {
                top: ((self.visible_start as f32) * self.item_height),
                left: 0.0,
                right: 0.0,
                bottom: 0.0,
            });

        let scrollable = Scrollable::with_direction(
            content,
            scrollable::Direction::Both {
                vertical: Properties::default(),
                horizontal: Properties::default(),
            },
        )
        .id(self.id.clone())
        .on_scroll(|v| LogFileMessage::ScrollChanged(v))
        //.height(Length::Shrink)
        .width(Length::Fill);

        scrollable.into()
    }
}
