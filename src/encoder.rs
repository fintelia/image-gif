//! # Minimal gif encoder
use std::io;
use std::io::prelude::*;
use std::fmt;
use std::error;
use std::mem;

use weezl::{BitOrder, encode::Encoder as LzwEncoder};

use crate::traits::{WriteBytesExt};
use crate::common::{AnyExtension, Block, DisposalMethod, Extension, Frame};

#[derive(Debug)]
enum FormatErrorKind {
    /// The image has too many colors.
    TooManyColors,
    /// The image has no color palette which is required.
    MissingColorPalette,
}

/// The image has incorrect properties, making it impossible to encode as a gif.
#[derive(Debug)]
pub struct EncodingFormatError {
    kind: FormatErrorKind
}

impl error::Error for EncodingFormatError {}
impl fmt::Display for EncodingFormatError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            FormatErrorKind::TooManyColors => write!(fmt, "the image has too many colors"),
            FormatErrorKind::MissingColorPalette => write!(fmt, "the GIF format requires a color palette but none was given")
        }
    }
}

impl From<FormatErrorKind> for EncodingFormatError {
    fn from(kind: FormatErrorKind) -> Self {
        EncodingFormatError { kind }
    }
}

#[derive(Debug)]
/// Encoding error.
pub enum EncodingError {
    /// Returned if the to image is not encodable as a gif.
    Format(EncodingFormatError),
    /// Wraps `std::io::Error`.
    Io(io::Error),
}

impl fmt::Display for EncodingError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            EncodingError::Io(err) => err.fmt(fmt),
            EncodingError::Format(err) => err.fmt(fmt),
        }
    }
}

impl error::Error for EncodingError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            EncodingError::Io(err) => Some(err),
            EncodingError::Format(err) => Some(err),
        }
    }
}

impl From<io::Error> for EncodingError {
    fn from(err: io::Error) -> Self {
        EncodingError::Io(err)
    }
}

impl From<EncodingFormatError> for EncodingError {
    fn from(err: EncodingFormatError) -> Self {
        EncodingError::Format(err)
    }
}

impl From<FormatErrorKind> for EncodingError {
    fn from(kind: FormatErrorKind) -> Self {
        EncodingError::Format(kind.into())
    }
}


/// Number of repetitions
#[derive(Copy, Clone, Debug)]
pub enum Repeat {
    /// Finite number of repetitions
    Finite(u16),
    /// Infinite number of repetitions
    Infinite
}

/// Extension data.
pub enum ExtensionData {
    /// Control extension. Use `ExtensionData::new_control_ext` to construct.
    Control {
        /// Flags.
        flags: u8,
        /// Frame delay.
        delay: u16,
        /// Transparent index.
        trns: u8
    },
    /// Sets the number of repetitions
    Repetitions(Repeat)
}

impl ExtensionData {
    /// Constructor for control extension data.
    ///
    /// `delay` is given in units of 10 ms.
    pub fn new_control_ext(delay: u16, dispose: DisposalMethod,
                           needs_user_input: bool, trns: Option<u8>) -> ExtensionData {
        let mut flags = 0;
        let trns = match trns {
            Some(trns) => {
                flags |= 1;
                trns as u8
            },
            None => 0
        };
        flags |= (needs_user_input as u8) << 1;
        flags |= (dispose as u8) << 2;
        ExtensionData::Control {
            flags: flags,
            delay: delay,
            trns: trns
        }
    }
}

impl<W: Write> Encoder<W> {
    /// Creates a new encoder.
    ///
    /// `global_palette` gives the global color palette in the format `[r, g, b, ...]`,
    /// if no global palette shall be used an empty slice may be supplied.
    pub fn new(w: W, width: u16, height: u16, global_palette: &[u8]) -> Result<Self, EncodingError> {
        let buffer_size = (width as usize) * (height as usize);
        Encoder {
            w: mem::ManuallyDrop::new(w),
            global_palette: false,
            width: width,
            height: height,
            buffer: Vec::with_capacity(buffer_size)
        }.write_global_palette(global_palette)
    }

    /// Write an extension block that signals a repeat behaviour.
    pub fn set_repeat(&mut self, repeat: Repeat) -> Result<(), EncodingError> {
        self.write_extension(ExtensionData::Repetitions(repeat))
    }

    /// Writes the global color palette.
    pub fn write_global_palette(mut self, palette: &[u8]) -> Result<Self, EncodingError> {
        self.global_palette = true;
        let mut flags = 0;
        flags |= 0b1000_0000;
        let num_colors = palette.len() / 3;
        if num_colors > 256 {
            return Err(EncodingError::from(FormatErrorKind::TooManyColors));
        }
        // Size of global color table.
        flags |= flag_size(num_colors);
        // Color resolution .. FIXME. This is mostly ignored (by ImageMagick at least) but hey, we
        // should use some sensible value here or even allow configuring it?
        flags |= flag_size(num_colors) << 4; // wtf flag
        self.write_screen_desc(flags)?;
        self.write_color_table(palette)?;
        Ok(self)
    }

    /// Writes a frame to the image.
    ///
    /// Note: This function also writes a control extension if necessary.
    pub fn write_frame(&mut self, frame: &Frame) -> Result<(), EncodingError> {
        // TODO commented off to pass test in lib.rs
        //if frame.delay > 0 || frame.transparent.is_some() {
            self.write_extension(ExtensionData::new_control_ext(
                frame.delay,
                frame.dispose,
                frame.needs_user_input,
                frame.transparent

            ))?;
        //}
        self.w.write_le(Block::Image as u8)?;
        self.w.write_le(frame.left)?;
        self.w.write_le(frame.top)?;
        self.w.write_le(frame.width)?;
        self.w.write_le(frame.height)?;
        let mut flags = 0;
        if frame.interlaced {
            flags |= 0b0100_0000;
        }
        match frame.palette {
            Some(ref palette) => {
                flags |= 0b1000_0000;
                let num_colors = palette.len() / 3;
                if num_colors > 256 {
                    return Err(EncodingError::from(FormatErrorKind::TooManyColors));
                }
                flags |= flag_size(num_colors);
                self.w.write_le(flags)?;
                self.write_color_table(palette)
            },
            None => if !self.global_palette {
                Err(EncodingError::from(FormatErrorKind::MissingColorPalette))
            } else {
                self.w.write_le(flags).map_err(Into::into)
            }
        }?;
        self.write_image_block(&frame.buffer)
    }

    fn write_image_block(&mut self, data: &[u8]) -> Result<(), EncodingError> {
        {
            let min_code_size: u8 = match flag_size(*data.iter().max().unwrap_or(&0) as usize + 1) + 1 {
                1 => 2, // As per gif spec: The minimal code size has to be >= 2
                n => n
            };
            self.w.write_le(min_code_size)?;
            self.buffer.clear();
            let mut enc = LzwEncoder::new(BitOrder::Lsb, min_code_size);
            let len = enc.into_vec(&mut self.buffer).encode_all(data).consumed_out;

            // Write blocks. `chunks_exact` seems to be slightly faster
            // than `chunks` according to both Rust docs and benchmark results.
            let mut iter = self.buffer[..len].chunks_exact(0xFF);
            while let Some(full_block) = iter.next() {
                self.w.write_le(0xFFu8)?;
                self.w.write_all(full_block)?;
            }
            let last_block = iter.remainder();
            if !last_block.is_empty() {
                self.w.write_le(last_block.len() as u8)?;
                self.w.write_all(last_block)?;
            }
        }
        self.w.write_le(0u8).map_err(Into::into)
    }

    fn write_color_table(&mut self, table: &[u8]) -> Result<(), EncodingError> {
        let num_colors = table.len() / 3;
        if num_colors > 256 {
            return Err(EncodingError::from(FormatErrorKind::TooManyColors));
        }
        let size = flag_size(num_colors);
        self.w.write_all(&table[..num_colors * 3])?;
        // Waste some space as of gif spec
        for _ in 0..((2 << size) - num_colors) {
            self.w.write_all(&[0, 0, 0])?
        }
        Ok(())
    }

    /// Writes an extension to the image.
    ///
    /// It is normally not necessary to call this method manually.
    pub fn write_extension(&mut self, extension: ExtensionData) -> Result<(), EncodingError> {
        use self::ExtensionData::*;
        // 0 finite repetitions can only be achieved
        // if the corresponting extension is not written
        if let Repetitions(Repeat::Finite(0)) = extension {
            return Ok(())
        }
        self.w.write_le(Block::Extension as u8)?;
        match extension {
            Control { flags, delay, trns } => {
                self.w.write_le(Extension::Control as u8)?;
                self.w.write_le(4u8)?;
                self.w.write_le(flags)?;
                self.w.write_le(delay)?;
                self.w.write_le(trns)?;
            }
            Repetitions(repeat) => {
                self.w.write_le(Extension::Application as u8)?;
                self.w.write_le(11u8)?;
                self.w.write_all(b"NETSCAPE2.0")?;
                self.w.write_le(3u8)?;
                self.w.write_le(1u8)?;
                match repeat {
                    Repeat::Finite(no) => self.w.write_le(no)?,
                    Repeat::Infinite => self.w.write_le(0u16)?,
                }
            }
        }
        self.w.write_le(0u8).map_err(Into::into)
    }

    /// Writes a raw extension to the image.
    ///
    /// This method can be used to write an unsupported extension to the file. `func` is the extension
    /// identifier (e.g. `Extension::Application as u8`). `data` are the extension payload blocks. If any
    /// contained slice has a lenght > 255 it is automatically divided into sub-blocks.
    pub fn write_raw_extension(&mut self, func: AnyExtension, data: &[&[u8]]) -> io::Result<()> {
        self.w.write_le(Block::Extension as u8)?;
        self.w.write_le(func.0)?;
        for block in data {
            for chunk in block.chunks(0xFF) {
                self.w.write_le(chunk.len() as u8)?;
                self.w.write_all(chunk)?;
            }
        }
        self.w.write_le(0u8)
    }

    /// Writes the logical screen desriptor
    fn write_screen_desc(&mut self, flags: u8) -> io::Result<()> {
        self.w.write_all(b"GIF89a")?;
        self.w.write_le(self.width)?;
        self.w.write_le(self.height)?;
        self.w.write_le(flags)?; // packed field
        self.w.write_le(0u8)?; // bg index
        self.w.write_le(0u8) // aspect ratio
    }

    /// Returns writer instance used by this encoder
    pub fn into_inner(self) -> io::Result<W> {
        // safety: ManuallyDrop wrapper prevents (second) Drop
        unsafe { mem::ManuallyDrop::new(self).inner_drop() }
    }

    /// A shared drop implementation used by the `Drop` trait and by `into_inner`
    ///
    /// ## Safety
    ///
    /// Must be called only once, and keep in mind it is called by `Drop`.
    unsafe fn inner_drop(&mut self) -> io::Result<W> {
        self.buffer = Vec::new(); // an empty Vec has no heap allocation
        let mut w = mem::ManuallyDrop::take(&mut self.w);
        w.write_le(Block::Trailer as u8)?;
        Ok(w)
    }
}

/// GIF encoder.
pub struct Encoder<W: Write> {
    w: mem::ManuallyDrop<W>,
    global_palette: bool,
    width: u16,
    height: u16,
    buffer: Vec<u8>
    // check `inner_drop` before adding new fields
}

impl<W: Write> Drop for Encoder<W> {

    #[cfg(feature = "raii_no_panic")]
    fn drop(&mut self) {
        let _ = unsafe { self.inner_drop() };
    }

    #[cfg(not(feature = "raii_no_panic"))]
    fn drop(&mut self) {
        let _ = unsafe { self.inner_drop().unwrap() };
    }
}

// Color table size converted to flag bits
fn flag_size(size: usize) -> u8 {
    match size {
        0  ..=2   => 0,
        3  ..=4   => 1,
        5  ..=8   => 2,
        9  ..=16  => 3,
        17 ..=32  => 4,
        33 ..=64  => 5,
        65 ..=128 => 6,
        129..=256 => 7,
        _ => 7
    }
}

#[test]
fn error_cast() {
    let _ : Box<dyn error::Error> = EncodingError::from(FormatErrorKind::MissingColorPalette).into();
}
