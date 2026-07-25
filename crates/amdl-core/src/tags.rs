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

/// Minimal, player-relevant tag view used by scan/covers/tag ops.
#[derive(Debug, Default, Clone)]
pub struct Basic {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub has_cover: bool,
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
    }
}

/// Track duration in whole seconds (from the decoded audio properties).
pub fn duration_secs(path: &Path) -> Option<u64> {
    let tagged = open(path)?;
    Some(tagged.properties().duration().as_secs())
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
