//! The artwork at full resolution through the kitty graphics
//! protocol, in terminals that answer the protocol's query: kitty, Ghostty,
//! WezTerm, and Herdr with `kitty_graphics` turned on. The picture is laid
//! over the block-character version, which stays underneath, so a terminal
//! that accepts the picture but cannot show it still shows the splash.
use ratatui::layout::Rect;
use std::io::{self, Write};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// Original 1254 by 1254 RGB artwork; no palette reduction or runtime image processor.
static PNG: &[u8] = include_bytes!("../../../assets/daedalus.png");
const SOURCE_SIDE: u16 = 1254;
/// The number this program gives its one picture.
const ID: u32 = 7183;
/// Base64 bytes per escape sequence, as the protocol allows.
const CHUNK: usize = 4096;
/// The query: a one-pixel test picture, a cell size request and, last,
/// device attributes, which every terminal answers.
#[cfg(unix)]
const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Kitty {
    /// One cell's width and height in pixels, or in proportion when the
    /// terminal does not say.
    pub cell: (f32, f32),
}

/// Whether to ask at all. `DAEDALUS_GRAPHICS=0` never asks and `=kitty`
/// always does; otherwise only terminals known to answer are asked, so a
/// late reply can never arrive as typed keys.
pub fn wanted(get: impl Fn(&str) -> Option<String>) -> bool {
    match get("DAEDALUS_GRAPHICS").as_deref() {
        Some("0") => false,
        Some("kitty") => true,
        _ => {
            get("HERDR_ENV").as_deref() == Some("1")
                || get("KITTY_WINDOW_ID").is_some()
                || get("TERM").is_some_and(|t| t == "xterm-kitty" || t == "xterm-ghostty")
                || get("TERM_PROGRAM").is_some_and(|t| t == "ghostty" || t == "WezTerm")
        }
    }
}

/// Ask the terminal, already in raw mode and before anything else reads
/// its input, whether it shows kitty graphics and how large a cell is.
#[cfg(unix)]
pub fn probe(timeout: Duration) -> Option<Kitty> {
    let mut out = io::stdout();
    out.write_all(QUERY).ok()?;
    out.flush().ok()?;
    let replies = parse(&read_replies(timeout));
    replies.kitty.then(|| Kitty {
        cell: replies.cell.or_else(window_cell).unwrap_or((1.0, 2.0)),
    })
}

// Windows Terminal does not expose Kitty images; keep the themed cell renderer.
#[cfg(windows)]
pub fn probe(_timeout: Duration) -> Option<Kitty> {
    None
}

/// Cell size from the window's size in pixels, when the terminal reports it.
#[cfg(unix)]
fn window_cell() -> Option<(f32, f32)> {
    let size = crossterm::terminal::window_size().ok()?;
    (size.width > 0 && size.height > 0 && size.columns > 0 && size.rows > 0).then(|| {
        (
            f32::from(size.width) / f32::from(size.columns),
            f32::from(size.height) / f32::from(size.rows),
        )
    })
}

/// Read the terminal's replies until device attributes arrive or time runs out.
#[cfg(unix)]
fn read_replies(timeout: Duration) -> Vec<u8> {
    use std::os::fd::AsRawFd;
    let fd = io::stdin().as_raw_fd();
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    while !parse(&bytes).attributes {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = left.as_millis().clamp(1, 1000) as libc::c_int;
        // SAFETY: one valid pollfd, and a buffer of the length passed.
        if unsafe { libc::poll(&mut poll, 1, millis) } <= 0 {
            break;
        }
        let mut chunk = [0u8; 512];
        let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if read <= 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read as usize]);
    }
    bytes
}

#[derive(Debug, Default, PartialEq)]
#[cfg(any(unix, test))]
struct Replies {
    kitty: bool,
    cell: Option<(f32, f32)>,
    attributes: bool,
}

#[cfg(any(unix, test))]
fn parse(bytes: &[u8]) -> Replies {
    let text = String::from_utf8_lossy(bytes);
    // CSI 6 ; height ; width t
    let cell = text.split("\x1b[6;").nth(1).and_then(|rest| {
        let mut numbers = rest[..rest.find('t')?].split(';');
        let height: f32 = numbers.next()?.parse().ok()?;
        let width: f32 = numbers.next()?.parse().ok()?;
        (height > 0.0 && width > 0.0).then_some((width, height))
    });
    // CSI ? digits and semicolons c
    let attributes = text.match_indices("\x1b[?").any(|(i, _)| {
        text[i + 3..]
            .chars()
            .find(|c| !(c.is_ascii_digit() || *c == ';'))
            == Some('c')
    });
    Replies {
        kitty: text.contains("\x1b_Gi=31;OK"),
        cell,
        attributes,
    }
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let n = group
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= group.len() {
                ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Send the picture once, to be shown later by its number. `q=2` keeps the
/// terminal from answering, so nothing reaches the input.
pub fn transmit(out: &mut impl Write) -> io::Result<()> {
    let data = base64(PNG);
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(CHUNK).collect();
    for (n, chunk) in chunks.iter().enumerate() {
        let more = u8::from(n + 1 < chunks.len());
        if n == 0 {
            write!(out, "\x1b_Ga=t,f=100,t=d,i={ID},q=2,m={more};")?;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    out.flush()
}

/// Show the picture scaled over `area`, above the text there.
pub fn place(out: &mut impl Write, area: Rect) -> io::Result<()> {
    place_crop(out, area, None)
}

/// Update only the cached image placement; never resend pixels during motion.
pub fn place_crop(out: &mut impl Write, area: Rect, crop: Option<Rect>) -> io::Result<()> {
    if area.width == 0 || area.height == 0 {
        return Ok(());
    }
    if let Some(crop) = crop {
        if crop.width == 0
            || crop.height == 0
            || crop.right() > SOURCE_SIDE
            || crop.bottom() > SOURCE_SIDE
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid artwork crop",
            ));
        }
        write!(
            out,
            "\x1b7\x1b[{};{}H\x1b_Ga=p,i={ID},p=1,x={},y={},w={},h={},c={},r={},C=1,z=1,q=2\x1b\\\x1b8",
            area.y + 1,
            area.x + 1,
            crop.x,
            crop.y,
            crop.width,
            crop.height,
            area.width,
            area.height
        )?;
        return out.flush();
    }
    write!(
        out,
        "\x1b7\x1b[{};{}H\x1b_Ga=p,i={ID},p=1,c={},r={},C=1,z=1,q=2\x1b\\\x1b8",
        area.y + 1,
        area.x + 1,
        area.width,
        area.height
    )?;
    out.flush()
}

/// Take the picture off the screen; with `forget`, free its data too.
pub fn remove(out: &mut impl Write, forget: bool) -> io::Result<()> {
    let what = if forget { 'I' } else { 'i' };
    write!(out, "\x1b_Ga=d,d={what},i={ID},q=2\x1b\\")?;
    out.flush()
}

fn valid_cell(cell: (f32, f32)) -> (f32, f32) {
    if cell.0.is_finite() && cell.1.is_finite() && cell.0 > 0.0 && cell.1 > 0.0 {
        cell
    } else {
        (1.0, 2.0)
    }
}
/// The cells the picture covers: as wide as the block-character picture in
/// `frame` (see `backdrop::placement`), as tall as a square needs with
/// these cells, and centered.
pub fn cover(frame: Rect, cell: (f32, f32)) -> Rect {
    if frame.width == 0 || frame.height == 0 {
        return frame;
    }
    let cell = valid_cell(cell);
    let side = (f32::from(frame.width)).min(f32::from(frame.height) * 2.0);
    let mut columns = (side.round() as u16).clamp(1, frame.width.max(1));
    let mut rows = (side * cell.0 / cell.1).round() as u16;
    if rows > frame.height {
        rows = frame.height;
        columns = ((f32::from(rows) * cell.1 / cell.0).round() as u16).min(columns);
    }
    let rows = rows.max(1);
    Rect::new(
        frame.x + (frame.width - columns) / 2,
        frame.y + (frame.height - rows) / 2,
        columns,
        rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_terminals_known_to_answer_are_asked() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(wanted(env(&[("HERDR_ENV", "1")])));
        assert!(wanted(env(&[("TERM", "xterm-kitty")])));
        assert!(wanted(env(&[("TERM_PROGRAM", "ghostty")])));
        assert!(!wanted(env(&[("TERM", "xterm-256color")])));
        assert!(!wanted(env(&[("TERM_PROGRAM", "tmux")])));
        assert!(!wanted(env(&[
            ("HERDR_ENV", "1"),
            ("DAEDALUS_GRAPHICS", "0")
        ])));
        assert!(wanted(env(&[("DAEDALUS_GRAPHICS", "kitty")])));
    }

    #[test]
    fn replies_are_read_in_any_order_and_end_at_device_attributes() {
        let all = parse(b"\x1b_Gi=31;OK\x1b\\\x1b[6;18;9t\x1b[?62;22;52c");
        assert_eq!(
            all,
            Replies {
                kitty: true,
                cell: Some((9.0, 18.0)),
                attributes: true
            }
        );
        // A terminal without graphics answers device attributes only.
        assert_eq!(
            parse(b"\x1b[?1;2c"),
            Replies {
                attributes: true,
                ..Replies::default()
            }
        );
        // Not finished yet: a refused picture, no attributes.
        let partial = parse(b"\x1b_Gi=31;ENOTSUPPORTED\x1b\\\x1b[?2026;2$y");
        assert!(!partial.kitty && !partial.attributes);
        // A malformed size is ignored.
        assert_eq!(parse(b"\x1b[6;0;9t\x1b[?6c").cell, None);
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0, 0, 0]), "AAAA");
        assert_eq!(base64(&[255, 254, 253]), "//79");
    }

    #[test]
    fn the_picture_is_sent_in_chunks_then_placed_and_removed() {
        let mut out = Vec::new();
        transmit(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let sequences: Vec<&str> = text.split("\x1b\\").filter(|s| !s.is_empty()).collect();
        assert!(sequences[0].starts_with("\x1b_Ga=t,f=100,t=d,i=7183,q=2,m=1;"));
        assert!(sequences.last().unwrap().starts_with("\x1b_Gm=0;"));
        assert!(
            sequences[1..sequences.len() - 1]
                .iter()
                .all(|s| s.starts_with("\x1b_Gm=1;"))
        );
        let payload: usize = sequences
            .iter()
            .map(|s| s.len() - s.find(';').unwrap() - 1)
            .sum();
        assert_eq!(payload, PNG.len().div_ceil(3) * 4);
        assert!(
            sequences
                .iter()
                .all(|s| s.len() - s.find(';').unwrap() - 1 <= CHUNK)
        );
        // Original RGB pixels and dimensions are retained, not the indexed splash asset.
        assert_eq!(u32::from_be_bytes(PNG[16..20].try_into().unwrap()), 1254);
        assert_eq!(u32::from_be_bytes(PNG[20..24].try_into().unwrap()), 1254);
        assert_eq!(&PNG[24..26], &[8, 2]);
        // The picture really is a PNG.
        assert_eq!(&PNG[..8], b"\x89PNG\r\n\x1a\n");
        let mut out = Vec::new();
        place(&mut out, Rect::new(30, 1, 80, 40)).unwrap();
        assert_eq!(
            out,
            b"\x1b7\x1b[2;31H\x1b_Ga=p,i=7183,p=1,c=80,r=40,C=1,z=1,q=2\x1b\\\x1b8"
        );
        let mut out = Vec::new();
        remove(&mut out, true).unwrap();
        assert_eq!(out, b"\x1b_Ga=d,d=I,i=7183,q=2\x1b\\");
    }

    #[test]
    fn invalid_source_rectangle_is_rejected() {
        let mut out = Vec::new();
        assert!(
            place_crop(
                &mut out,
                Rect::new(0, 0, 80, 40),
                Some(Rect::new(1200, 0, 200, 200))
            )
            .is_err()
        );
        assert!(out.is_empty());
    }
    #[test]
    fn empty_panes_keep_their_frame() {
        for frame in [Rect::new(0, 0, 0, 40), Rect::new(0, 0, 40, 0)] {
            assert_eq!(cover(frame, (9.0, 18.0)), frame);
        }
    }
    #[test]
    fn the_picture_covers_the_block_picture_square_in_pixels() {
        // Cells twice as tall as wide: exactly the block picture's box.
        assert_eq!(
            cover(Rect::new(0, 1, 140, 40), (9.0, 18.0)),
            Rect::new(30, 1, 80, 40)
        );
        // Taller cells: fewer rows for the same width.
        assert_eq!(
            cover(Rect::new(0, 1, 140, 40), (10.0, 22.0)),
            Rect::new(30, 3, 80, 36)
        );
        // Shorter cells would need more rows than there are: narrower instead.
        let r = cover(Rect::new(0, 1, 140, 40), (10.0, 18.0));
        assert_eq!((r.height, r.width), (40, 72));
        assert_eq!(r.x, 34);
        // A narrow frame fits its width.
        assert_eq!(
            cover(Rect::new(0, 0, 40, 40), (9.0, 18.0)),
            Rect::new(0, 10, 40, 20)
        );
    }
}
