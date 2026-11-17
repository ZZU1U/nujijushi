//! Split a post's media into the messages Telegram can actually accept.
//!
//! Bot API rules: an album holds 2..=10 items, and a media group cannot mix
//! photos with videos. Photos around a video therefore stay in their own albums,
//! and videos are sent as individual messages with a poster frame.

/// Maximum items in one `sendMediaGroup` call.
pub const MAX_ALBUM: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageShape {
    Single,
    Album,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageGroup {
    pub shape: MessageShape,
    /// Indices into the post's media list.
    pub indices: Vec<usize>,
    /// True when the caption belongs on this message.
    pub caption: bool,
}

impl MessageGroup {
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// Plan the messages for a post, preserving media order.
///
/// The caption goes on the first message only - Telegram renders a single
/// captioned item of an album as the album caption.
pub fn plan_messages(is_video: &[bool]) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();
    let mut photos: Vec<usize> = Vec::new();

    let flush = |photos: &mut Vec<usize>, groups: &mut Vec<MessageGroup>| {
        while photos.len() > MAX_ALBUM {
            let rest = photos.split_off(MAX_ALBUM);
            let chunk = std::mem::replace(photos, rest);
            groups.push(MessageGroup {
                shape: MessageShape::Album,
                indices: chunk,
                caption: false,
            });
        }
        if photos.len() > 1 {
            let chunk = std::mem::take(photos);
            groups.push(MessageGroup {
                shape: MessageShape::Album,
                indices: chunk,
                caption: false,
            });
        } else if let Some(index) = photos.pop() {
            groups.push(MessageGroup {
                shape: MessageShape::Single,
                indices: vec![index],
                caption: false,
            });
        }
    };

    for (index, video) in is_video.iter().enumerate() {
        if *video {
            flush(&mut photos, &mut groups);
            groups.push(MessageGroup {
                shape: MessageShape::Single,
                indices: vec![index],
                caption: false,
            });
        } else {
            photos.push(index);
        }
    }
    flush(&mut photos, &mut groups);

    if let Some(first) = groups.first_mut() {
        first.caption = true;
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes(groups: &[MessageGroup]) -> Vec<(MessageShape, usize)> {
        groups.iter().map(|g| (g.shape, g.len())).collect()
    }

    #[test]
    fn single_photo() {
        let groups = plan_messages(&[false]);
        assert_eq!(shapes(&groups), vec![(MessageShape::Single, 1)]);
        assert!(groups[0].caption);
    }

    #[test]
    fn two_photos_make_an_album() {
        let groups = plan_messages(&[false, false]);
        assert_eq!(shapes(&groups), vec![(MessageShape::Album, 2)]);
    }

    #[test]
    fn ten_is_one_album_eleven_splits() {
        let ten = vec![false; 10];
        assert_eq!(
            shapes(&plan_messages(&ten)),
            vec![(MessageShape::Album, 10)]
        );

        let eleven = vec![false; 11];
        assert_eq!(
            shapes(&plan_messages(&eleven)),
            vec![(MessageShape::Album, 10), (MessageShape::Single, 1)]
        );
    }

    #[test]
    fn twenty_four_photos_chunk_into_ten_ten_four() {
        let groups = plan_messages(&[false; 24]);
        assert_eq!(
            shapes(&groups),
            vec![
                (MessageShape::Album, 10),
                (MessageShape::Album, 10),
                (MessageShape::Album, 4)
            ]
        );
        assert_eq!(groups.iter().filter(|g| g.caption).count(), 1);
        assert!(groups[0].caption);
        // all indices covered exactly once, in order
        let flat: Vec<usize> = groups.iter().flat_map(|g| g.indices.clone()).collect();
        assert_eq!(flat, (0..24).collect::<Vec<_>>());
    }

    #[test]
    fn videos_are_not_mixed_into_albums() {
        let groups = plan_messages(&[false, false, true, false]);
        assert_eq!(
            shapes(&groups),
            vec![
                (MessageShape::Album, 2),
                (MessageShape::Single, 1),
                (MessageShape::Single, 1)
            ]
        );
        assert_eq!(groups[1].indices, vec![2]);
        assert_eq!(groups[2].indices, vec![3]);
    }

    #[test]
    fn only_videos() {
        let groups = plan_messages(&[true, true]);
        assert_eq!(
            shapes(&groups),
            vec![(MessageShape::Single, 1), (MessageShape::Single, 1)]
        );
    }

    #[test]
    fn empty_post_yields_no_messages() {
        assert!(plan_messages(&[]).is_empty());
    }

    #[test]
    fn preserves_order_across_chunks() {
        let mut flags = vec![false; 12];
        flags[11] = true;
        let groups = plan_messages(&flags);
        let flat: Vec<usize> = groups.iter().flat_map(|g| g.indices.clone()).collect();
        assert_eq!(flat, (0..12).collect::<Vec<_>>());
        assert_eq!(groups.last().unwrap().shape, MessageShape::Single);
    }
}
