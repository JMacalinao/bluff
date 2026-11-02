//! Tray icons shipped with bluff.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Disconnected,
    Laptop,
    Ipad,
    Pairing,
}

/// One size of an icon: width, height and ARGB32 pixels in network byte order.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub argb: &'static [u8],
}

/// The frames of an icon, as built by brandkit/build.py.
pub fn pixmaps(icon: Icon) -> Vec<Frame> {
    let mut data: &'static [u8] = match icon {
        Icon::Disconnected => include_bytes!("bluff/icons/tray-disconnected.argb"),
        Icon::Laptop => include_bytes!("bluff/icons/tray-laptop.argb"),
        Icon::Ipad => include_bytes!("bluff/icons/tray-ipad.argb"),
        Icon::Pairing => include_bytes!("bluff/icons/tray-pairing.argb"),
    };
    let mut frames = Vec::new();
    while let Some((header, rest)) = data.split_first_chunk::<8>() {
        let width = u32::from_be_bytes(header[..4].try_into().unwrap());
        let height = u32::from_be_bytes(header[4..].try_into().unwrap());
        let (argb, rest) = rest.split_at((width * height * 4) as usize);
        frames.push(Frame {
            width,
            height,
            argb,
        });
        data = rest;
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_pixmaps() {
        for icon in [Icon::Disconnected, Icon::Laptop, Icon::Ipad, Icon::Pairing] {
            let frames = pixmaps(icon);
            let widths: Vec<u32> = frames.iter().map(|f| f.width).collect();
            assert_eq!(widths, [16, 22, 24, 32, 48, 64]);
            assert!(
                frames
                    .iter()
                    .all(|f| f.argb.len() == (f.width * f.height * 4) as usize)
            );
        }
    }
}
