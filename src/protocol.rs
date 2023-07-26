use core::{str::from_utf8};
use coolssh::{Run, RunEvent};
use super::internals::{Result, Error, Write};

pub enum PacketLine<'a> {
    String(&'a str),
    Bytes(&'a [u8]),
    FlushPacket,
    DelimiterPacket,
    ResponseEndPacket,
}

pub struct GitProtocol<'a> {
    run: Run<'a>,
    receive_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    to_skip: usize,
}

impl<'a> GitProtocol<'a> {
    pub fn new(run: Run<'a>) -> GitProtocol<'a> {
        Self {
            run,
            receive_buffer: Vec::new(),
            send_buffer: Vec::new(),
            to_skip: 0,
        }
    }

    pub fn read_line(&mut self) -> Result<Option<&[u8]>> {
        fn parse_len(bytes: &[u8]) -> Option<usize> {
            let hex_len = from_utf8(bytes).ok()?;
            usize::from_str_radix(hex_len, 16).ok()
        }

        self.receive_buffer.drain(0..self.to_skip);
        self.to_skip = 0;

        loop {
            if let Some(slice) = self.receive_buffer.get(..4) {
                let len = parse_len(slice).ok_or(Error::GitProtocolError)?;
                if len < 4 {
                    self.to_skip = 4;
                    break Ok(None);
                } else if self.receive_buffer.len() >= len {
                    self.to_skip = len;
                    break match self.receive_buffer.get(4..len) {
                        Some(data) => Ok(Some(data)),
                        None => Err(Error::GitProtocolError),
                    };
                }
            }

            match self.run.poll()? {
                RunEvent::None => (),
                RunEvent::Data(data) => self.receive_buffer.extend_from_slice(data),
                RunEvent::ExtDataStderr(data) => log::warn!("Remote stderr: {}", from_utf8(data).unwrap()),
                e => {
                    log::error!("Unexpected RunEvent: {:?}", e);
                    break Err(Error::GitProtocolError);
                },
            }
        }
    }

    pub fn read_line_str(&mut self) -> Result<Option<&str>> {
        Ok(match self.read_line()? {
            Some(b) => Some(from_utf8(b).ok().ok_or(Error::GitProtocolError)?.trim()),
            None => None,
        })
    }

    pub fn write_lines(&mut self, lines: &[PacketLine]) -> Result<()> {
        for line in lines {
            match line {
                PacketLine::String(string) => {
                    write!(&mut self.send_buffer, "{:04x}{}", string.len() + 4, string)
                },
                PacketLine::Bytes(bytes) => {
                    write!(&mut self.send_buffer, "{:04x}", bytes.len() + 4).unwrap();
                    self.send_buffer.write(bytes).map(|_| ())
                },
                PacketLine::FlushPacket => write!(&mut self.send_buffer, "0000"),
                PacketLine::DelimiterPacket => write!(&mut self.send_buffer, "0001"),
                PacketLine::ResponseEndPacket => write!(&mut self.send_buffer, "0002"),
            }.unwrap();
        }

        self.run.write(&self.send_buffer, Error::GitProtocolError)?;

        self.send_buffer.clear();

        Ok(())
    }

    pub fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        self.run.write(data, Error::GitProtocolError)
    }

    pub fn wait_for_exit(&mut self, ignore_data: bool) -> Result<()> {
        loop {
            match self.run.poll()? {
                RunEvent::None => (),
                RunEvent::Data(_) if ignore_data => (),
                RunEvent::Stopped(Some(0)) => break Ok(()),
                RunEvent::ExtDataStderr(data) => log::warn!("Remote stderr: {}", from_utf8(data).unwrap()),
                e => {
                    log::error!("Unexpected RunEvent: {:?}", e);
                    break Err(Error::GitProtocolError);
                },
            }
        }
    }
}
