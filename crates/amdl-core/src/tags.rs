//! Tag + cover I/O via `lofty`. This is what makes convert *fidelity* possible:
//! ffmpeg's stream-copy of an MP4 `covr`/ID3 `APIC` into Opus is unreliable
//! (players often don't see it), so after transcoding we re-embed the source
//! cover as a spec-correct Opus `METADATA_BLOCK_PICTURE` and strip iTunes junk.
use anyhow::{Context, Result};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag, TagItem, TagType};
use std::path::Path;

/// amdl's own tag marking a track as instrumental with high confidence, so
/// `lyrics` skips it (no network, no re-scan) on every later run. A plain custom
/// Vorbis comment (`AMDL_INSTRUMENTAL=1`); ignored by players, preserved by our
/// junk-strip.
pub const INSTRUMENTAL_KEY: &str = "AMDL_INSTRUMENTAL";

/// Minimal, player-relevant tag view used by scan/covers/tag ops.
#[derive(Debug, Default, Clone)]
pub struct Basic {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub has_cover: bool,
    /// Track carries amdl's `AMDL_INSTRUMENTAL` marker (see [`INSTRUMENTAL_KEY`]).
    pub instrumental_marked: bool,
}

fn open(path: &Path) -> Option<lofty::file::TaggedFile> {
    Probe::open(path).ok()?.read().ok()
}

/// Read the front cover (or the first picture) from an audio file, if any.
pub fn read_cover(path: &Path) -> Option<Picture> {
    let tagged = open(path)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let pics = tag.pictures();
    pics.iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pics.first())
        .cloned()
}

/// Read basic tags + whether any picture is embedded.
pub fn read_basic(path: &Path) -> Basic {
    let Some(tagged) = open(path) else {
        return Basic::default();
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Basic::default();
    };
    Basic {
        title: tag.title().map(|s| s.to_string()),
        artist: tag.artist().map(|s| s.to_string()),
        album: tag.album().map(|s| s.to_string()),
        album_artist: tag
            .get_string(&ItemKey::AlbumArtist)
            .map(|s| s.to_string()),
        has_cover: !tag.pictures().is_empty(),
        instrumental_marked: is_instrumental_mark(tag),
    }
}

/// Whether a tag carries amdl's `AMDL_INSTRUMENTAL=1` marker.
fn is_instrumental_mark(tag: &Tag) -> bool {
    tag.get_string(&ItemKey::Unknown(INSTRUMENTAL_KEY.to_string()))
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Stamp `AMDL_INSTRUMENTAL=1` on a file (idempotent), preserving every other
/// tag. Marks a track amdl has judged instrumental so future `lyrics` runs skip
/// it. Callers should journal the write (see `journal::edit`).
pub fn set_instrumental_mark(path: &Path) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read()?;
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(TagType::VorbisComments));
    }
    let tag = tagged.primary_tag_mut().expect("tag present");
    // `insert` runs `re_map`, which rejects `ItemKey::Unknown` (no built-in
    // mapping) and would silently drop the item; `insert_unchecked` stores it,
    // and VorbisComments' save keeps it (the key passes lofty's `verify_key`).
    tag.insert_unchecked(TagItem::new(ItemKey::Unknown(INSTRUMENTAL_KEY.to_string()), ItemValue::Text("1".into())));
    tagged
        .save_to_path(path, WriteOptions::default())
        .with_context(|| format!("save instrumental mark to {}", path.display()))?;
    Ok(())
}

/// Track duration in whole seconds (from the decoded audio properties).
pub fn duration_secs(path: &Path) -> Option<u64> {
    let tagged = open(path)?;
    Some(tagged.properties().duration().as_secs())
}

/// Everything `stats` needs from a file, read in a **single** lofty open (tags,
/// cover presence, embedded lyrics, and audio properties). `None` = the file
/// couldn't be probed (unreadable / unsupported).
#[derive(Debug, Default, Clone)]
pub struct Meta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub has_cover: bool,
    pub lyrics: Option<String>,
    pub duration_secs: u64,
    /// Audio bitrate in kbps (falls back to the container's overall bitrate).
    pub bitrate_kbps: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// Probe a file once for the full [`Meta`] view. See [`Meta`].
pub fn read_meta(path: &Path) -> Option<Meta> {
    let tagged = open(path)?;
    let props = tagged.properties();
    let mut m = Meta {
        duration_secs: props.duration().as_secs(),
        bitrate_kbps: props.audio_bitrate().or_else(|| props.overall_bitrate()),
        sample_rate: props.sample_rate(),
        channels: props.channels(),
        ..Default::default()
    };
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        m.title = tag.title().map(|s| s.to_string());
        m.artist = tag.artist().map(|s| s.to_string());
        m.album = tag.album().map(|s| s.to_string());
        m.album_artist = tag.get_string(&ItemKey::AlbumArtist).map(|s| s.to_string());
        m.has_cover = !tag.pictures().is_empty();
        m.lyrics = tag.get_string(&ItemKey::Lyrics).map(|s| s.to_string());
    }
    Some(m)
}

pub fn has_cover(path: &Path) -> bool {
    open(path)
        .map(|t| t.tags().iter().any(|tag| !tag.pictures().is_empty()))
        .unwrap_or(false)
}

/// iTunes/MP4 atoms that are meaningless in Opus and pollute the library.
fn is_junk_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("itunsmpb")
        || k.contains("itunnorm")
        || k.contains("itunes_cddb")
        || k.contains("itunmovi")
        || k.contains("com.apple.itunes")
        || k == "major_brand"
        || k == "minor_version"
        || k == "compatible_brands"
        || k == "encoder"
        || k == "handler_name"
        || k == "vendor_id"
}

fn strip_junk(tag: &mut Tag) {
    tag.retain(|item| match item.key() {
        ItemKey::Unknown(k) => !is_junk_key(k),
        _ => true,
    });
}

/// After ffmpeg writes an Opus file: strip junk tags and, if the Opus carries no
/// picture, embed `cover` as a proper `METADATA_BLOCK_PICTURE`. Idempotent —
/// re-running on an already-good file changes nothing.
pub fn finalize_opus(opus: &Path, cover: Option<&Picture>) -> Result<bool> {
    let mut tagged = Probe::open(opus)
        .with_context(|| format!("open {}", opus.display()))?
        .read()
        .with_context(|| format!("read {}", opus.display()))?;

    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(TagType::VorbisComments));
    }
    let tag = tagged.primary_tag_mut().expect("tag present");

    strip_junk(tag);
    let mut embedded = false;
    if tag.pictures().is_empty() {
        if let Some(c) = cover {
            tag.push_picture(c.clone());
            embedded = true;
        }
    }
    tagged
        .save_to_path(opus, WriteOptions::default())
        .with_context(|| format!("save tags to {}", opus.display()))?;
    Ok(embedded)
}

/// Force-set the front cover (replacing any existing pictures) and strip junk —
/// used by the paste-a-URL flow, where the operator has chosen the album's art.
pub fn set_cover(path: &Path, cover: &Picture) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read()?;
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(TagType::VorbisComments));
    }
    let tag = tagged.primary_tag_mut().expect("tag present");
    strip_junk(tag);
    while !tag.pictures().is_empty() {
        tag.remove_picture(0);
    }
    tag.push_picture(cover.clone());
    tagged
        .save_to_path(path, WriteOptions::default())
        .with_context(|| format!("save cover to {}", path.display()))?;
    Ok(())
}

/// Set title/artist/album on a file (only the provided fields), preserving the
/// rest. Used by `identify` to write an AcoustID match.
pub fn write_fields(path: &Path, title: Option<&str>, artist: Option<&str>, album: Option<&str>) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read()?;
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(TagType::VorbisComments));
    }
    let tag = tagged.primary_tag_mut().expect("tag present");
    if let Some(t) = title {
        tag.set_title(t.to_string());
    }
    if let Some(a) = artist {
        tag.set_artist(a.to_string());
    }
    if let Some(al) = album {
        tag.set_album(al.to_string());
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .with_context(|| format!("save tags to {}", path.display()))?;
    Ok(())
}

/// Group a track with its album siblings: set `album`, and — if the album is a
/// compilation — `albumartist=Various Artists` + `compilation=1`. Used by
/// `recover`: a re-acquired track carries Apple's *own* album tag, which often
/// differs from the compilation/folder it belongs to, so without this it splits
/// into a lone one-track album in a tag-grouping player.
pub fn set_album_grouping(path: &Path, album: &str, compilation: bool) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read()?;
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(TagType::VorbisComments));
    }
    let tag = tagged.primary_tag_mut().expect("tag present");
    tag.set_album(album.to_string());
    if compilation {
        tag.insert(TagItem::new(ItemKey::AlbumArtist, ItemValue::Text("Various Artists".into())));
        tag.insert(TagItem::new(ItemKey::FlagCompilation, ItemValue::Text("1".into())));
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .with_context(|| format!("save tags to {}", path.display()))?;
    Ok(())
}

/// Read the embedded lyrics (`LYRICS` Vorbis comment / equivalent), if any.
pub fn read_lyrics(path: &Path) -> Option<String> {
    let tagged = open(path)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    tag.get_string(&ItemKey::Lyrics).map(|s| s.to_string())
}

/// Embed `lyrics` (plain or LRC-format synced text) into the file's `LYRICS`
/// tag, replacing any existing value and preserving every other field. Used by
/// `lyrics --embed`; the caller decides *whether* to write (never-downgrade).
pub fn set_lyrics(path: &Path, lyrics: &str) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read()?;
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(TagType::VorbisComments));
    }
    let tag = tagged.primary_tag_mut().expect("tag present");
    tag.insert(TagItem::new(ItemKey::Lyrics, ItemValue::Text(lyrics.to_string())));
    tagged
        .save_to_path(path, WriteOptions::default())
        .with_context(|| format!("save lyrics to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_junk_key;

    #[test]
    fn strips_itunes_junk_keeps_real_tags() {
        for k in ["iTunSMPB", "iTunNORM", "----:com.apple.iTunes:CT", "major_brand", "encoder"] {
            assert!(is_junk_key(k), "{k} should be junk");
        }
        for k in ["ARTIST", "ALBUM", "TITLE", "MUSICBRAINZ_ALBUMID", "ALBUMARTIST"] {
            assert!(!is_junk_key(k), "{k} should be kept");
        }
    }
}
