//! Sparse, bounded playlist metadata. Logical rows never allocate placeholders.
use super::model;
use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;

pub(super) const PAGE_SIZE: usize = 15;
const MAX_PAGES: usize = 8;

struct Page {
    tracks: Vec<model::Track>,
    offsets: Vec<usize>,
    end: usize,
    next: Option<u32>,
}

#[derive(Default)]
pub(super) struct PlaylistBuffer {
    total: usize,
    pages: BTreeMap<usize, Page>,
    recency: VecDeque<usize>,
    pub(super) revision: usize,
}

impl PlaylistBuffer {
    pub(super) fn new(total: usize) -> Self {
        Self {
            total,
            ..Self::default()
        }
    }
    pub(super) fn len(&self) -> usize {
        self.total
    }
    pub(super) fn is_empty(&self) -> bool {
        self.total == 0
    }
    pub(super) fn tracks(&self) -> impl Iterator<Item = &model::Track> {
        self.pages.values().flat_map(|page| page.tracks.iter())
    }
    fn page(&self, index: usize) -> Option<(&usize, &Page)> {
        self.pages
            .range(..=index)
            .next_back()
            .filter(|(_, page)| index < page.end)
    }
    pub(super) fn get(&self, index: usize) -> Option<&model::Track> {
        let (_, page) = self.page(index)?;
        page.offsets
            .binary_search(&index)
            .ok()
            .and_then(|slot| page.tracks.get(slot))
    }
    pub(super) fn contains(&self, index: usize) -> bool {
        self.page(index).is_some()
    }
    pub(super) fn insert(&mut self, offset: u32, result: model::PlaylistTrackPage) {
        self.total = result.total as usize;
        let start = offset as usize;
        let end = result
            .next_offset
            .map_or(self.total, |next| next as usize)
            .max(start + result.tracks.len());
        let offsets = if result.source_offsets.len() == result.tracks.len() {
            result
                .source_offsets
                .into_iter()
                .map(|offset| offset as usize)
                .collect()
        } else {
            (start..start + result.tracks.len()).collect()
        };
        self.pages.insert(
            start,
            Page {
                tracks: result.tracks,
                offsets,
                end,
                next: result.next_offset,
            },
        );
        self.touch(start);
        while self.pages.len() > MAX_PAGES {
            if let Some(old) = self.recency.pop_front() {
                self.pages.remove(&old);
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }
    fn touch(&mut self, offset: usize) {
        self.recency.retain(|old| *old != offset);
        self.recency.push_back(offset);
    }
    pub(super) fn missing_offset(&mut self, range: Range<usize>) -> Option<u32> {
        let mut keys = [0; MAX_PAGES];
        let mut count = 0;
        for (start, page) in &self.pages {
            if *start < range.end && page.end > range.start {
                keys[count] = *start;
                count += 1;
            }
        }
        for key in keys.into_iter().take(count) {
            self.touch(key);
        }
        (range.start..range.end.min(self.total))
            .find(|index| !self.contains(*index))
            .and_then(|index| u32::try_from(index / PAGE_SIZE * PAGE_SIZE).ok())
    }
    /// Build only a contiguous loaded playback window. The backend continues
    /// at the exact raw Spotify cursor, including omitted/unavailable entries.
    pub(super) fn playback(&self, index: usize) -> Option<(Vec<model::Track>, usize, Option<u32>)> {
        let (start, _) = self.page(index)?;
        let mut tracks =
            Vec::with_capacity(self.pages.values().map(|page| page.tracks.len()).sum());
        let mut playing = None;
        let mut offset = *start;
        let next = loop {
            let page = self.pages.get(&offset)?;
            for (raw, track) in page.offsets.iter().zip(&page.tracks) {
                if *raw == index {
                    playing = Some(tracks.len());
                }
                tracks.push(track.clone());
            }
            match page.next {
                Some(next)
                    if self.pages.contains_key(&(next as usize)) && next as usize > offset =>
                {
                    offset = next as usize
                }
                next => break next,
            }
        };
        Some((tracks, playing?, next))
    }
    pub(super) fn missing_start(&self) -> Option<u32> {
        let mut offset = 0;
        loop {
            let Some(page) = self.pages.get(&(offset as usize)) else {
                return Some(offset);
            };
            if !page.tracks.is_empty() {
                return None;
            }
            let next = page.next?;
            if next <= offset {
                return None;
            }
            offset = next;
        }
    }
    pub(super) fn first_playback(&self) -> Option<(Vec<model::Track>, usize, Option<u32>)> {
        let mut offset = 0;
        loop {
            let page = self.pages.get(&offset)?;
            if let Some(first) = page.offsets.first() {
                return self.playback(*first);
            }
            let next = page.next? as usize;
            if next <= offset {
                return None;
            }
            offset = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::track;

    #[test]
    fn huge_logical_playlists_keep_only_eight_recent_metadata_pages() {
        let mut buffer = PlaylistBuffer::new(100_000);
        for offset in (0..600).step_by(PAGE_SIZE) {
            buffer.insert(
                offset as u32,
                model::PlaylistTrackPage {
                    source_offsets: vec![],
                    tracks: (offset..offset + 15).map(track).collect(),
                    next_offset: Some(offset as u32 + 15),
                    total: 100_000,
                },
            );
        }
        assert_eq!(buffer.len(), 100_000);
        assert_eq!(buffer.tracks().count(), MAX_PAGES * PAGE_SIZE);
        assert!(buffer.get(0).is_none());
        assert_eq!(buffer.missing_offset(99_991..100_000), Some(99_990));
    }

    #[test]
    fn sparse_playback_preserves_holes_duplicates_and_the_raw_continuation_cursor() {
        let mut buffer = PlaylistBuffer::new(1000);
        buffer.insert(
            975,
            model::PlaylistTrackPage {
                source_offsets: vec![976, 979],
                tracks: vec![track(1), track(1)],
                next_offset: Some(990),
                total: 1000,
            },
        );
        buffer.insert(
            990,
            model::PlaylistTrackPage {
                source_offsets: vec![995],
                tracks: vec![track(2)],
                next_offset: None,
                total: 1000,
            },
        );
        assert!(buffer.contains(977));
        assert!(buffer.get(977).is_none());
        let (tracks, index, next) = buffer.playback(979).unwrap();
        assert_eq!(index, 1);
        assert_eq!(tracks, vec![track(1), track(1), track(2)]);
        assert_eq!(next, None);
    }
}
