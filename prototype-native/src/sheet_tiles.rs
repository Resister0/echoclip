//! 11 Nov 2026: the player's seek pictures as 60 small images, cut once from the clip's 10 x 6 sheet.
//! The filmstrip (~25-30 pictures across) and the seek preview used to draw cut-outs of the WHOLE sheet
//! (`source-clip`). Slint's OpenGL renderer only shares one texture between Image items when the picture comes
//! from a file; a picture made from pixels (the sheet) is uploaded again for every Image item that shows it. So
//! every filmstrip picture held its own copy of the 3200x1080 sheet (~14 MB each): measured on an AMD card,
//! ~400 MB of graphics memory with the player open (690-710 MB, Home 161), and ~200 MB of it stayed after closing
//! (the driver keeps the freed space) - also in the tray, i.e. taken from the game. With tiles each picture is
//! 320x180 (~0.2 MB), uploaded once. Same idea as the editor's filmstrip (edui.rs build_tiles).
//!
//! API (player + Home's inline player): `set(pl, img, tw, th)` = show this sheet (sets Play.sheet / -tw / -th /
//! -ok as before, then fills Play.tiles from a worker); `clear(pl)` = player closed: let everything go.
use crate::Play;
use std::cell::Cell;

thread_local! {
    /// newest request: a cut that finishes after a newer one (or after clear) is dropped
    static GEN: Cell<u64> = const { Cell::new(0) };
}

/// Show `img` (the clip's 10 x 6 sheet, pictures `tw` x `th`) in the player: Play.sheet as before (the editor cuts
/// its own tiles from it, osd.rs uses its pixels), plus Play.tiles for the filmstrip + seek preview.
pub fn set(pl: &Play, img: slint::Image, tw: i32, th: i32) {
    pl.set_sheet(img.clone());
    pl.set_sheet_tw(tw);
    pl.set_sheet_th(th);
    pl.set_sheet_ok(true);
    let gen = GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let (tw, th) = (tw.max(1) as u32, th.max(1) as u32);
    // the pixels go to the worker as they are (to_rgb8 / to_rgba8 share the buffer, no copy here)
    let src = img.to_rgb8().map(Ok).or_else(|| img.to_rgba8().map(Err));
    let Some(src) = src else { return };
    std::thread::spawn(move || {
        let tiles: Option<Vec<Tile>> = match src {
            Ok(b) => cut(b.as_bytes(), b.width(), b.height(), 3, tw, th).map(|t| t.into_iter().map(Tile::Rgb).collect()),
            Err(b) => cut(b.as_bytes(), b.width(), b.height(), 4, tw, th).map(|t| t.into_iter().map(Tile::Rgba).collect()),
        };
        let Some(tiles) = tiles else { return };
        let _ = slint::invoke_from_event_loop(move || {
            if GEN.with(|g| g.get()) != gen {
                return; // a newer clip, or the player closed meanwhile
            }
            let Some(a) = crate::UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())) else { return };
            let imgs: Vec<slint::Image> = tiles
                .into_iter()
                .map(|t| match t {
                    Tile::Rgb((px, w, h)) => slint::Image::from_rgb8(slint::SharedPixelBuffer::clone_from_slice(&px, w, h)),
                    Tile::Rgba((px, w, h)) => slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(&px, w, h)),
                })
                .collect();
            use slint::ComponentHandle;
            a.global::<Play>().set_tiles(slint::ModelRc::new(slint::VecModel::from(imgs)));
        });
    });
}

/// Player closed: no sheet, no tiles (their textures go with them).
pub fn clear(pl: &Play) {
    GEN.with(|g| g.set(g.get() + 1));
    pl.set_sheet(slint::Image::default());
    pl.set_sheet_ok(false);
    pl.set_tiles(slint::ModelRc::default());
}

enum Tile {
    Rgb((Vec<u8>, u32, u32)),
    Rgba((Vec<u8>, u32, u32)),
}

/// cut the sheet (`bpp` bytes per pixel, rows packed) into its 60 pictures of tw x th, row by row
fn cut(px: &[u8], w: u32, h: u32, bpp: usize, tw: u32, th: u32) -> Option<Vec<(Vec<u8>, u32, u32)>> {
    if (w as usize) * (h as usize) * bpp > px.len() {
        return None;
    }
    let mut out = Vec::with_capacity(60);
    for f in 0..60u32 {
        let (x, y) = ((f % 10) * tw, (f / 10) * th);
        if x + tw > w || y + th > h {
            break;
        }
        let mut t = Vec::with_capacity((tw * th) as usize * bpp);
        for row in y..y + th {
            let s = (row as usize * w as usize + x as usize) * bpp;
            t.extend_from_slice(&px[s..s + tw as usize * bpp]);
        }
        out.push((t, tw, th));
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::cut;
    #[test]
    fn cuts_sixty_pictures_in_order() {
        // 10 x 6 pictures of 2 x 1 px, RGB; each picture's pixels hold its number
        let (tw, th) = (2u32, 1u32);
        let (w, h) = (10 * tw, 6 * th);
        let mut px = vec![0u8; (w * h * 3) as usize];
        for f in 0..60u32 {
            let (x, y) = ((f % 10) * tw, (f / 10) * th);
            for dx in 0..tw {
                let i = ((y * w + x + dx) * 3) as usize;
                px[i..i + 3].fill(f as u8);
            }
        }
        let t = cut(&px, w, h, 3, tw, th).unwrap();
        assert_eq!(t.len(), 60);
        for (f, (p, a, b)) in t.iter().enumerate() {
            assert_eq!((*a, *b), (tw, th));
            assert!(p.iter().all(|&v| v == f as u8));
        }
        assert!(cut(&px[..10], w, h, 3, tw, th).is_none());
    }
}
