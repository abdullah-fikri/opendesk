//! Clipboard text and file lists. The platform backend is chosen at compile time.

/// An image read from the clipboard.
pub enum ClipImage {
    /// The registered "PNG" format, as is.
    Png(Vec<u8>),
    /// CF_DIB: a BITMAPINFOHEADER, optional masks and palette, then the pixels.
    Dib(Vec<u8>),
}

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;
