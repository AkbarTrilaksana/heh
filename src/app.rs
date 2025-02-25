use std::{
    cmp,
    error::Error,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    process,
};

use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};

use arboard::Clipboard;

use crate::{
    label::{LabelHandler, LABEL_TITLES},
    screen::{
        ClickedComponent::{self, *},
        ScreenHandler,
    },
};

#[derive(PartialEq)]
pub(crate) enum FocusedEditor {
    Ascii,
    Hex,
}

#[derive(PartialEq)]
pub(crate) enum Nibble {
    Beginning,
    End,
}

pub(crate) struct Application {
    file: File,
    contents: Vec<u8>,
    start_address: usize,
    offset: usize,
    display: ScreenHandler,
    labels: LabelHandler,
    last_click: ClickedComponent,
    clipboard: Option<Clipboard>,
    focused_editor: FocusedEditor,
    nibble: Nibble,
}

impl Application {
    pub(crate) fn new(mut file: File) -> Result<Application, Box<dyn Error>> {
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .expect("Reading the contents of the file was interrupted.");
        if contents.is_empty() {
            eprintln!("heh does not support editing empty files");
            process::exit(1);
        }
        let mut labels = LabelHandler::new(&contents);
        let clipboard = Clipboard::new().ok();
        if clipboard.is_none() {
            labels.notification = String::from("Can't find clipboard!");
        }
        Ok(Application {
            file,
            contents,
            start_address: 0,
            offset: 0,
            display: ScreenHandler::new()?,
            labels,
            last_click: Unclickable,
            clipboard,
            focused_editor: FocusedEditor::Hex,
            nibble: Nibble::Beginning,
        })
    }
    pub(crate) fn run(&mut self) -> Result<(), Box<dyn Error>> {
        self.display.setup()?;
        loop {
            self.render_display()?;
            if !self.handle_input()? {
                break;
            }
        }
        self.display.teardown()?;
        Ok(())
    }
    fn render_display(&mut self) -> Result<(), Box<dyn Error>> {
        self.display.render(
            &self.contents,
            self.start_address,
            self.offset,
            &self.labels,
            &self.focused_editor,
        )?;
        Ok(())
    }
    fn handle_input(&mut self) -> Result<bool, Box<dyn Error>> {
        let event = event::read()?;
        match event {
            Event::Key(key) => {
                match key.code {
                    // Directional inputs that move the selected offset
                    KeyCode::Left => {
                        self.offset = self.offset.saturating_sub(1);
                        self.offset_change_epilogue();
                    }
                    KeyCode::Right => {
                        self.offset =
                            cmp::min(self.offset.saturating_add(1), self.contents.len() - 1);
                        self.offset_change_epilogue();
                    }
                    KeyCode::Up => {
                        self.offset = self
                            .offset
                            .saturating_sub(self.display.comp_layouts.bytes_per_line);
                        self.offset_change_epilogue();
                    }
                    KeyCode::Down => {
                        self.offset = cmp::min(
                            self.offset
                                .saturating_add(self.display.comp_layouts.bytes_per_line),
                            self.contents.len() - 1,
                        );
                        self.offset_change_epilogue();
                    }

                    // Input that removes bytes
                    KeyCode::Backspace => {
                        if self.offset > 0 {
                            self.contents.remove(self.offset - 1);
                            self.offset = self.offset.saturating_sub(1);
                            self.offset_change_epilogue();
                        }
                    }
                    KeyCode::Delete => {
                        if self.contents.len() > 1 {
                            self.contents.remove(self.offset);
                            self.offset = self.offset.saturating_sub(1);
                            self.offset_change_epilogue();
                        }
                    }

                    // Character Input and Shortcuts
                    KeyCode::Char(char) => {
                        // A flag to indicate if a user wasn't trying to modify a byte
                        // i.e., CNTRLs shouldn't save and then modify the file
                        let mut special_command = false;
                        match char {
                            'q' => {
                                if key.modifiers == KeyModifiers::CONTROL {
                                    return Ok(false);
                                }
                            }
                            's' => {
                                if key.modifiers == KeyModifiers::CONTROL {
                                    special_command = true;
                                    self.file.seek(SeekFrom::Start(0))?;
                                    self.file.write_all(&self.contents)?;
                                    self.file.set_len(self.contents.len() as u64)?;
                                    self.labels.notification = String::from("Saved!");
                                }
                            }
                            '=' => {
                                if key.modifiers == KeyModifiers::ALT {
                                    special_command = true;
                                    self.labels.update_stream_length(cmp::min(
                                        self.labels.get_stream_length() + 1,
                                        64,
                                    ));
                                    self.labels.update_streams(&self.contents[self.offset..]);
                                }
                            }
                            '-' => {
                                if key.modifiers == KeyModifiers::ALT {
                                    special_command = true;
                                    self.labels.update_stream_length(cmp::max(
                                        self.labels.get_stream_length().saturating_sub(1),
                                        0,
                                    ));
                                    self.labels.update_streams(&self.contents[self.offset..]);
                                }
                            }
                            _ => {}
                        }
                        if !special_command
                            && key.modifiers | KeyModifiers::SHIFT == KeyModifiers::SHIFT
                        {
                            match self.focused_editor {
                                FocusedEditor::Ascii => {
                                    self.contents[self.offset] = char as u8;
                                    self.offset = cmp::min(
                                        self.offset.saturating_add(1),
                                        self.contents.len() - 1,
                                    );
                                    self.offset_change_epilogue();
                                }
                                FocusedEditor::Hex => {
                                    if char.is_ascii_hexdigit() {
                                        // This can probably be optimized...
                                        if self.nibble == Nibble::Beginning {
                                            // Change the first character of the selected byte
                                            let mut src = char.to_string();
                                            src.push(
                                                format!("{:02X}", self.contents[self.offset])
                                                    .chars()
                                                    .last()
                                                    .unwrap(),
                                            );
                                            let changed =
                                                u8::from_str_radix(src.as_str(), 16).unwrap();
                                            self.contents[self.offset] = changed;
                                            // Move to next nibble
                                            self.nibble = Nibble::End;
                                        } else {
                                            let mut src =
                                                format!("{:02X}", self.contents[self.offset])
                                                    .chars()
                                                    .take(1)
                                                    .collect::<String>();
                                            src.push(char);
                                            let changed =
                                                u8::from_str_radix(src.as_str(), 16).unwrap();
                                            self.contents[self.offset] = changed;

                                            // Move to the next byte
                                            self.offset = cmp::min(
                                                self.offset.saturating_add(1),
                                                self.contents.len() - 1,
                                            );
                                            self.offset_change_epilogue();
                                        }
                                    } else {
                                        self.labels.notification = format!("Invalid Hex: {char}");
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Mouse(mouse) => {
                let component = self
                    .display
                    .identify_clicked_component(mouse.row, mouse.column);
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    self.last_click = component;
                    match self.last_click {
                        HexTable => {
                            self.focused_editor = FocusedEditor::Hex;
                            self.nibble = Nibble::Beginning;
                        }
                        AsciiTable => {
                            self.focused_editor = FocusedEditor::Ascii;
                        }
                        Label(_) => {}
                        Unclickable => {}
                    }
                } else if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                    match component {
                        HexTable => {}
                        AsciiTable => {}
                        Label(i) => {
                            if self.last_click == component {
                                // Put string into clipboard
                                if let Some(clipboard) = self.clipboard.as_mut() {
                                    clipboard
                                        .set_text(self.labels[LABEL_TITLES[i]].clone())
                                        .unwrap();
                                    self.labels.notification =
                                        format!("{} copied!", LABEL_TITLES[i]);
                                } else {
                                    self.labels.notification =
                                        String::from("Can't find clipboard!");
                                }
                            }
                        }
                        Unclickable => {}
                    }
                }
                //self.labels.notification = format!("{:?} at ({}, {})", mouse.kind, mouse.row, mouse.column);
            }
            _ => {}
        }
        Ok(true)
    }
    // Puts the offset back into the display
    fn adjust_offset(&mut self) {
        let line_adjustment = ((self.offset as f32 - self.start_address as f32)
            / self.display.comp_layouts.bytes_per_line as f32)
            .floor()
            .abs() as usize;
        let bytes_per_screen =
            self.display.comp_layouts.bytes_per_line * self.display.comp_layouts.lines_per_screen;
        if self.offset < self.start_address {
            self.start_address = self
                .start_address
                .saturating_sub(self.display.comp_layouts.bytes_per_line * line_adjustment);
        } else if self.offset >= self.start_address + (bytes_per_screen)
            && self.start_address + self.display.comp_layouts.bytes_per_line < self.contents.len()
        {
            self.start_address = self.start_address.saturating_add(
                self.display.comp_layouts.bytes_per_line
                    * (line_adjustment + 1 - self.display.comp_layouts.lines_per_screen),
            );
        }
        self.labels.offset = format!("{:#X}", self.offset);
    }
    fn offset_change_epilogue(&mut self) {
        self.nibble = Nibble::Beginning;
        self.labels.update_all(&self.contents[self.offset..]);
        self.adjust_offset();
    }
}
