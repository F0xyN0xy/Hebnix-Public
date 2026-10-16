//! Read game frames without decoding text that the relay does not inspect.
use std::io::Read;
use tungstenite::error::{CapacityError, ProtocolError};
use tungstenite::protocol::frame::coding::{Control, Data, OpCode};
use tungstenite::protocol::frame::{Frame, FrameSocket};
use tungstenite::{Error, Result};

const MAX_FRAME_SIZE: usize = 16 << 20;
const MAX_MESSAGE_SIZE: usize = 64 << 20;

pub(super) struct GameFrames<S> {
    socket: FrameSocket<S>,
    fragmented_size: Option<usize>,
    closing: bool,
}

impl<S: Read> GameFrames<S> {
    pub(super) fn new(stream: S) -> Self {
        Self {
            socket: FrameSocket::new(stream),
            fragmented_size: None,
            closing: false,
        }
    }

    pub(super) fn can_send_requests(&self) -> bool {
        self.fragmented_size.is_none() && !self.closing
    }

    pub(super) fn read(&mut self) -> Result<Option<Frame>> {
        let Some(mut frame) = self.socket.read(Some(MAX_FRAME_SIZE))? else {
            return Ok(None);
        };
        let header = frame.header();
        if header.rsv1 || header.rsv2 || header.rsv3 {
            return Err(Error::Protocol(ProtocolError::NonZeroReservedBits));
        }
        if self.closing {
            return Err(Error::Protocol(ProtocolError::ReceivedAfterClosing));
        }
        let mask = header
            .mask
            .ok_or(Error::Protocol(ProtocolError::UnmaskedFrameFromClient))?;
        // FrameSocket returns the wire payload. Remove the game's mask before
        // the upstream WebSocket applies its own fresh client mask.
        for (index, byte) in frame.payload_mut().iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        frame.header_mut().mask = None;
        let header = frame.header();
        match header.opcode {
            OpCode::Control(control) => {
                if !header.is_final {
                    return Err(Error::Protocol(ProtocolError::FragmentedControlFrame));
                }
                if frame.payload().len() > 125 {
                    return Err(Error::Protocol(ProtocolError::ControlFrameTooBig));
                }
                match control {
                    Control::Reserved(code) => {
                        return Err(Error::Protocol(ProtocolError::UnknownControlFrameType(
                            code,
                        )));
                    }
                    Control::Close => self.closing = true,
                    Control::Ping | Control::Pong => {}
                }
            }
            OpCode::Data(data) => {
                let previous_size = match (data, self.fragmented_size) {
                    (Data::Continue, Some(size)) => size,
                    (Data::Continue, None) => {
                        return Err(Error::Protocol(ProtocolError::UnexpectedContinueFrame));
                    }
                    (_, Some(_)) => {
                        return Err(Error::Protocol(ProtocolError::ExpectedFragment(data)));
                    }
                    (Data::Reserved(code), None) => {
                        return Err(Error::Protocol(ProtocolError::UnknownDataFrameType(code)));
                    }
                    (Data::Text | Data::Binary, None) => 0,
                };
                let size = previous_size + frame.payload().len();
                if size > MAX_MESSAGE_SIZE {
                    return Err(Error::Capacity(CapacityError::MessageTooLong {
                        size,
                        max_size: MAX_MESSAGE_SIZE,
                    }));
                }
                self.fragmented_size = (!header.is_final).then_some(size);
            }
        }
        Ok(Some(frame))
    }
}

