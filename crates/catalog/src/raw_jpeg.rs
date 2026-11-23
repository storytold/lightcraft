//! Raw + JPEG pairs: a camera set to write both leaves `IMG_0001.ARW` and `IMG_0001.JPG` side by side. Such a
//! shot can be kept as one stack with the raw on top ([`Catalog::raw_jpeg_pair_ops`]), so a grid shows it once.

use std::collections::{BTreeMap, HashSet};

use crate::{Catalog, MediaKind, Op, Photo, PhotoId, Source, Stack};

/// Whether `ext` (no dot) names a JPEG file.
fn jpeg_ext(ext: &str) -> bool {
    ["jpg", "jpeg", "jpe"].iter().any(|e| ext.eq_ignore_ascii_case(e))
}

/// A file-backed photo's path without its extension, and the extension: what a raw and its JPEG share, and what
/// tells them apart. The folder is compared as [`crate::query::folder_key`] spells it, the file's stem as written
/// (as the camera wrote both). `None` for demo photos and for names without a stem or an extension.
fn stem_and_ext(p: &Photo) -> Option<(String, String)> {
    let Source::File { path } = &p.source else { return None };
    let key = crate::query::folder_key(path);
    let (stem, ext) = key.rsplit_once('.')?;
    // the dot is the file name's own, and not its first character (`a.b/file`, `.hidden`)
    let plain = !ext.is_empty() && !ext.contains('/') && !stem.is_empty() && !stem.ends_with('/');
    plain.then(|| (stem.to_string(), ext.to_string()))
}

/// A file name without its extension, lower-cased: a cheap first test before [`stem_and_ext`].
fn name_stem(name: &str) -> String {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem).to_ascii_lowercase()
}

/// The photos of one shot: the files in one folder that share a name but for the extension, in id order.
#[derive(Default)]
struct Shot {
    raws: Vec<PhotoId>,
    jpegs: Vec<PhotoId>,
}

impl Shot {
    fn is_pair(&self) -> bool {
        !self.raws.is_empty() && !self.jpegs.is_empty()
    }
    fn members(&self) -> Vec<PhotoId> {
        self.raws.iter().chain(&self.jpegs).copied().collect()
    }
}

/// The shot a photo belongs to and whether it is the raw (`true`) or the JPEG; `None` for anything else.
fn shot_of(p: &Photo) -> Option<(String, bool)> {
    match p.kind {
        MediaKind::Raw => stem_and_ext(p).map(|(stem, _)| (stem, true)),
        MediaKind::Image => stem_and_ext(p).filter(|(_, ext)| jpeg_ext(ext)).map(|(stem, _)| (stem, false)),
        MediaKind::Video => None,
    }
}

impl Catalog {
    /// The raws and JPEGs by shot (not deleted photos). Library photos and Local browse records of the same files
    /// are shots of their own: a stack never mixes the two. With `names`, only the files whose [`name_stem`] is
    /// one of them.
    fn shots(&self, names: Option<&HashSet<String>>) -> BTreeMap<(bool, String), Shot> {
        let mut shots: BTreeMap<(bool, String), Shot> = BTreeMap::new();
        for p in self.photos.values().filter(|p| !p.deleted) {
            if names.is_some_and(|names| !names.contains(&name_stem(&p.file_name))) {
                continue;
            }
            if let Some((stem, raw)) = shot_of(p) {
                let shot = shots.entry((p.local, stem)).or_default();
                if raw { shot.raws.push(p.id) } else { shot.jpegs.push(p.id) }
            }
        }
        shots
    }

    /// Ops that stack every raw with the JPEG beside it (same folder, same file name but for the extension):
    /// one collapsed stack per shot, the raw on top (its virtual copies come along). With `new`, only the shots
    /// one of those photos belongs to (an import asks about what it added). A shot is left alone when it is one
    /// stack already, or when one of its files is stacked with other photos (a bracket set, a burst): that
    /// grouping is someone's own.
    pub fn raw_jpeg_pair_ops(&mut self, new: Option<&[PhotoId]>) -> Vec<Op> {
        let names: Option<HashSet<String>> =
            new.map(|ids| ids.iter().filter_map(|id| self.photos.get(id)).map(|p| name_stem(&p.file_name)).collect());
        if names.as_ref().is_some_and(HashSet::is_empty) {
            return Vec::new();
        }
        let shots = self.shots(names.as_ref());
        let mut ops = Vec::new();
        for shot in shots.values().filter(|s| s.is_pair()) {
            let members = shot.members();
            if new.is_some_and(|ids| !members.iter().any(|m| ids.contains(m))) {
                continue;
            }
            let stacks: Vec<_> = members.iter().map(|m| self.stack_of(*m)).collect();
            let foreign = stacks.iter().flatten().any(|s| s.photos.iter().any(|p| !members.contains(p)));
            let one_stack = stacks.first().copied().flatten().map(|s| s.id);
            let together = one_stack.is_some() && stacks.iter().all(|s| s.map(|s| s.id) == one_stack);
            if foreign || together {
                continue;
            }
            let Some(top) = shot.raws.first() else { continue };
            ops.extend(self.group_ops(*top, &members, true));
        }
        ops
    }

    /// Whether a stack holds one shot's raws and JPEGs and nothing else, at least one of each: what
    /// [`Catalog::raw_jpeg_pair_ops`] makes. Its JPEGs.
    fn pair_stack_jpegs(&self, stack: &Stack) -> Option<Vec<PhotoId>> {
        let mut shot: Option<(bool, String)> = None;
        let (mut raws, mut jpegs) = (0usize, Vec::new());
        for id in &stack.photos {
            let p = self.photos.get(id)?;
            let (stem, raw) = shot_of(p)?;
            if raw {
                raws += 1
            } else {
                jpegs.push(*id)
            }
            if *shot.get_or_insert_with(|| (p.local, stem.clone())) != (p.local, stem) {
                return None;
            }
        }
        (raws > 0 && !jpegs.is_empty()).then_some(jpegs)
    }

    /// Whether `stack` is a raw + JPEG pair's ([`Catalog::raw_jpeg_pair_ops`]): a grouping the files' names give,
    /// not one somebody chose.
    pub(crate) fn is_raw_jpeg_stack(&self, stack: &Stack) -> bool {
        self.pair_stack_jpegs(stack).is_some()
    }

    /// Ops that take the JPEGs back out of the stacks [`Catalog::raw_jpeg_pair_ops`] makes: the stacks that hold
    /// one shot's raws and JPEGs and nothing else. A raw keeps the virtual copies it is stacked with.
    pub fn raw_jpeg_unpair_ops(&self) -> Vec<Op> {
        let jpegs: Vec<PhotoId> = self.stacks.values().filter_map(|stack| self.pair_stack_jpegs(stack)).flatten().collect();
        if jpegs.is_empty() { Vec::new() } else { self.remove_from_stacks_ops(&jpegs) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(files: &[&str]) -> (Catalog, Vec<PhotoId>) {
        let mut c = Catalog::default();
        let mut ids = Vec::new();
        for (i, path) in files.iter().enumerate() {
            let id = PhotoId(i as u64 + 1);
            let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
            let ext = name.rsplit('.').next().unwrap_or_default().to_uppercase();
            let mut p = Photo::new(id, Source::File { path: (*path).to_string() }, name, &ext, 60, 40, "2026-01-01T00:00:00");
            if ["ARW", "DNG", "CR3"].contains(&ext.as_str()) {
                p.kind = MediaKind::Raw;
            }
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            ids.push(id);
        }
        (c, ids)
    }

    fn apply(c: &mut Catalog, ops: Vec<Op>) {
        for op in ops {
            c.apply(op).unwrap();
        }
    }

    fn stacks(c: &Catalog) -> Vec<Vec<PhotoId>> {
        let mut all: Vec<Vec<PhotoId>> = c.stacks().map(|s| s.photos.clone()).collect();
        all.sort();
        all
    }

    #[test]
    fn a_raw_and_its_jpeg_become_one_collapsed_stack_with_the_raw_on_top() {
        let (mut c, ids) = cat(&[
            "/shoot/IMG_0001.JPG",
            "/shoot/IMG_0001.ARW",
            "/shoot/IMG_0002.jpeg",
            "/shoot/IMG_0003.DNG",
            "/other/IMG_0003.JPG",
            "/shoot/IMG_0004.CR3",
            "/shoot/IMG_0004.TIF",
        ]);
        let ops = c.raw_jpeg_pair_ops(None);
        apply(&mut c, ops);
        // a lone JPEG, a JPEG in another folder and a TIFF pair with nothing
        assert_eq!(stacks(&c), vec![vec![ids[1], ids[0]]]);
        assert!(c.stacks().all(|s| s.collapsed));
        assert_eq!(c.arrange_stacks(&ids), vec![ids[1], ids[2], ids[3], ids[4], ids[5], ids[6]], "the grid shows the shot once, as the raw");
        assert!(c.raw_jpeg_pair_ops(None).is_empty(), "stacked already: nothing to do");
        // and back
        let ops = c.raw_jpeg_unpair_ops();
        apply(&mut c, ops);
        assert!(stacks(&c).is_empty());
    }

    #[test]
    fn a_shot_stacked_with_other_photos_is_left_alone_both_ways() {
        let (mut c, ids) = cat(&["/shoot/IMG_1.ARW", "/shoot/IMG_1.JPG", "/shoot/IMG_2.ARW", "/shoot/IMG_2.JPG"]);
        // a bracket set of the two raws
        let op = c.group_ops(ids[0], &[ids[2]], true).unwrap();
        apply(&mut c, vec![op]);
        assert!(c.raw_jpeg_pair_ops(None).is_empty());
        assert!(c.raw_jpeg_unpair_ops().is_empty());
        assert_eq!(stacks(&c), vec![vec![ids[0], ids[2]]]);
    }

    #[test]
    fn an_import_asks_only_about_the_shots_it_added_to() {
        let (mut c, ids) = cat(&["/shoot/IMG_1.ARW", "/shoot/IMG_1.JPG", "/shoot/IMG_2.ARW", "/shoot/IMG_2.JPG"]);
        // the JPEG of the second shot arrived: its raw, catalogued earlier, joins it
        let ops = c.raw_jpeg_pair_ops(Some(&[ids[3]]));
        apply(&mut c, ops);
        assert_eq!(stacks(&c), vec![vec![ids[2], ids[3]]]);
        assert!(c.raw_jpeg_pair_ops(Some(&[])).is_empty());
    }

    #[test]
    fn odd_names_demo_and_deleted_photos_pair_with_nothing_and_local_records_pair_among_themselves() {
        let (mut c, ids) = cat(&[
            "/a.b/IMG",
            "/a.b/IMG.JPG",
            "/shoot/.ARW",
            "/shoot/.JPG",
            "C:\\shoot\\IMG_1.ARW",
            "c:/shoot/./IMG_1.jpg",
            "/gone/IMG_2.ARW",
            "/gone/IMG_2.JPG",
            "/local/IMG_3.ARW",
            "/local/IMG_3.JPG",
            "/mixed/IMG_4.ARW",
            "/mixed/IMG_4.JPG",
        ]);
        c.apply(Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(99), Source::Demo { scene: 1 }, "1.jpg", "JPEG", 60, 40, "2026-01-01T00:00:00")) })
            .unwrap();
        let mut change = |i: usize, f: &dyn Fn(&mut Photo)| {
            let mut p = (**c.photos.get(&ids[i]).unwrap()).clone();
            f(&mut p);
            c.photos.insert(ids[i], std::sync::Arc::new(p));
        };
        change(6, &|p| p.deleted = true);
        // a browsed folder's two records pair; a Local record and a library photo do not
        change(8, &|p| p.local = true);
        change(9, &|p| p.local = true);
        change(10, &|p| p.local = true);
        let ops = c.raw_jpeg_pair_ops(None);
        apply(&mut c, ops);
        assert_eq!(stacks(&c), vec![vec![ids[4], ids[5]], vec![ids[8], ids[9]]]);
    }

    /// Browsed Local records, old enough to be forgotten: two raw + JPEG pairs.
    fn browsed() -> (Catalog, Vec<PhotoId>) {
        let (mut c, ids) = cat(&["/card/IMG_1.ARW", "/card/IMG_1.JPG", "/card/IMG_2.ARW", "/card/IMG_2.JPG"]);
        for id in &ids {
            let mut p = (**c.photos.get(id).unwrap()).clone();
            p.local = true;
            p.set_local_baseline();
            c.photos.insert(*id, std::sync::Arc::new(p));
        }
        c.apply(Op::SetBrowsed { folder: "/card".into(), at: Some("2026-01-01T00:00:00".into()) }).unwrap();
        (c, ids)
    }

    #[test]
    fn a_pair_stack_does_not_keep_local_records_and_goes_with_them() {
        let (mut c, ids) = browsed();
        let ops = c.raw_jpeg_pair_ops(None);
        apply(&mut c, ops);
        assert_eq!(stacks(&c).len(), 2);
        let plan = c.forget_local_plan("2026-06-01T00:00:00", 30, &|_| false);
        assert_eq!(plan.evict, ids, "stacked as pairs, forgotten all the same");
        apply(&mut c, plan.ops());
        assert!(stacks(&c).is_empty(), "no stack points at a record that is gone");
        assert_eq!(c.photos().count(), 0);
    }

    #[test]
    fn a_pair_stack_that_loses_one_record_is_dissolved_and_a_chosen_stack_still_keeps_its_records() {
        let (mut c, ids) = browsed();
        let ops = c.raw_jpeg_pair_ops(None);
        apply(&mut c, ops);
        // the first raw is in use: only its JPEG goes, and the stack with it
        let plan = c.forget_local_plan("2026-06-01T00:00:00", 30, &|p| p.id == ids[0]);
        assert_eq!(plan.evict, vec![ids[1], ids[2], ids[3]]);
        apply(&mut c, plan.ops());
        assert!(stacks(&c).is_empty());
        assert!(c.photo(ids[0]).is_some());

        // two raws somebody stacked: kept, as before
        let (mut c, ids) = browsed();
        let op = c.group_ops(ids[0], &[ids[2]], true).unwrap();
        apply(&mut c, vec![op]);
        let plan = c.forget_local_plan("2026-06-01T00:00:00", 30, &|_| false);
        assert_eq!(plan.evict, vec![ids[1], ids[3]]);
        assert_eq!(c.forgettable_local(&ids), vec![ids[1], ids[3]]);
    }
}
