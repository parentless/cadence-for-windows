//! Small references into already-loaded library slices, never a duplicate index.
use super::*;

pub(super) const LIMIT: usize = 6;

#[derive(Clone)]
pub(super) struct Match {
    collection: usize,
    index: usize,
    provider: model::Provider,
    source_id: Arc<str>,
}

impl Match {
    pub fn resolve<'a>(&self, library: &'a library::Library) -> Option<&'a model::Track> {
        let slices = [
            library.favorites(),
            library.recently_played(),
            library.liked_tracks(),
        ];
        slices[self.collection].get(self.index).filter(|track| {
            track.provider == self.provider && track.source_id == self.source_id.as_ref()
        })
    }
}

pub(super) fn find(query: &str, sources: &[Arc<[model::Track]>; 3]) -> Vec<Match> {
    let query = query.to_lowercase();
    let words: Vec<_> = query.split_whitespace().take(8).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let contains = |text: &str, needle: &str| {
        if needle.is_ascii() {
            text.as_bytes()
                .windows(needle.len())
                .any(|part| part.eq_ignore_ascii_case(needle.as_bytes()))
        } else {
            text.to_lowercase().contains(needle)
        }
    };
    let mut matches: Vec<Match> = Vec::with_capacity(LIMIT);
    for (collection, tracks) in sources.iter().enumerate() {
        for (index, track) in tracks.iter().enumerate() {
            if !words.iter().all(|word| {
                contains(&track.title, word)
                    || contains(&track.artist, word)
                    || contains(&track.album, word)
            }) {
                continue;
            }
            if matches.iter().any(|found| {
                found.provider == track.provider && found.source_id.as_ref() == track.source_id
            }) {
                continue;
            }
            matches.push(Match {
                collection,
                index,
                provider: track.provider,
                source_id: Arc::from(track.source_id.as_str()),
            });
            if matches.len() == LIMIT {
                return matches;
            }
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::track;
    #[test]
    fn cached_matches_are_bounded_deduplicated_and_search_multiple_fields() {
        let mut song = track(0);
        song.title = "BLUE Train".into();
        song.artist = "John Coltrane".into();
        let sources = [
            vec![song.clone()].into(),
            vec![song.clone()].into(),
            (0..40)
                .map(|i| {
                    let mut t = song.clone();
                    t.source_id = format!("track-{i}");
                    t
                })
                .collect::<Vec<_>>()
                .into(),
        ];
        let result = find("blue coltrane", &sources);
        assert_eq!(result.len(), LIMIT);
        assert_eq!(result[0].collection, 0);
        assert_eq!(
            result
                .iter()
                .filter(|found| found.source_id.as_ref() == song.source_id)
                .count(),
            1
        );
        assert!(find("missing", &sources).is_empty());
    }
    #[test]
    fn unicode_and_empty_queries_are_handled() {
        let mut song = track(0);
        song.title = "ÉTÉ".into();
        let sources = [vec![song].into(), Arc::default(), Arc::default()];
        assert_eq!(find("été", &sources).len(), 1);
        assert!(find("  ", &sources).is_empty());
    }
}
