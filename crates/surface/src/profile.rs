//! Pages and profiles.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT.
//!
//! A **profile** (Default, Wedding, Portrait…) is a set of **pages**. Each page binds some
//! controls; a page belongs to the key group, the dial group or both ([`PageGroup`]), and each
//! group has its own current page, so the keypad and the dialpad page separately. Controls a page
//! leaves unbound fall back to the profile's **base** bindings (the Dialpad's buttons, typically).
//! A profile's **contexts** map what LightCraft shows (`crop`, `masking.mask`, `develop.color`,
//! `dialog.export` …, see [`crate::context`]) to the pages to show there.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::binding::Binding;
use crate::devices::{PageGroup, group_of};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Page {
    pub name: String,
    /// The groups this page fills (`[keys]`, `[dials]` or both). Empty = both.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<PageGroup>,
    /// Control id → binding.
    pub bindings: BTreeMap<String, Binding>,
    /// Shown only when its context comes up (a tool page): paging by hand skips it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

impl Page {
    pub fn new(name: &str, groups: &[PageGroup], bindings: impl IntoIterator<Item = (&'static str, Binding)>) -> Page {
        Page {
            name: name.to_string(),
            groups: groups.to_vec(),
            bindings: bindings.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            auto: false,
        }
    }

    /// Does this page fill `group`?
    pub fn in_group(&self, group: PageGroup) -> bool {
        self.groups.is_empty() || self.groups.contains(&group)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Bindings that hold on every page unless a page binds the same control.
    pub base: BTreeMap<String, Binding>,
    pub pages: Vec<Page>,
    /// Context key → page names to show there (at most one per group is used).
    pub contexts: BTreeMap<String, Vec<String>>,
}

impl Profile {
    pub fn page(&self, name: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.name == name)
    }

    /// Pages of a group a user pages through by hand (not the automatic tool pages), in order.
    pub fn pages_in(&self, group: PageGroup) -> impl Iterator<Item = &Page> {
        self.pages.iter().filter(move |p| p.in_group(group) && !p.auto)
    }

    /// The page after (`step` 1) or before (`-1`) `current` within `group`, wrapping.
    pub fn step_page(&self, group: PageGroup, current: Option<&str>, step: i32) -> Option<String> {
        let names: Vec<&str> = self.pages_in(group).map(|p| p.name.as_str()).collect();
        if names.is_empty() {
            return None;
        }
        let n = names.len() as i64;
        let i = current.and_then(|c| names.iter().position(|p| *p == c)).map_or(0, |i| i as i64);
        let j = (i + i64::from(step)).rem_euclid(n);
        names.get(usize::try_from(j).unwrap_or(0)).map(|s| s.to_string())
    }

    /// The binding of `control` with `pages` current (group → page name): the page of the
    /// control's group, else the base.
    pub fn binding<'a>(&'a self, control: &str, pages: &BTreeMap<PageGroup, String>) -> Option<&'a Binding> {
        let group = group_of(control);
        pages.get(&group).and_then(|name| self.page(name)).and_then(|p| p.bindings.get(control)).or_else(|| self.base.get(control))
    }

    /// The pages to show for `context`, by group: the context's own entry, else its parents'
    /// (`masking.mask.brush` → `masking.mask` → `masking`), else `default`.
    pub fn pages_for_context(&self, context: &str) -> BTreeMap<PageGroup, String> {
        self.context_pages(context, true)
    }

    /// As [`Profile::pages_for_context`]; without `default` when `include_default` is false (the
    /// pages this context asks for itself).
    pub fn context_pages(&self, context: &str, include_default: bool) -> BTreeMap<PageGroup, String> {
        let mut out = BTreeMap::new();
        for key in crate::context::fallbacks(context) {
            if !include_default && key == "default" {
                break;
            }
            if let Some(names) = self.contexts.get(&key) {
                for name in names {
                    let Some(page) = self.page(name) else { continue };
                    for g in PageGroup::ALL {
                        if page.in_group(g) {
                            out.entry(g).or_insert_with(|| page.name.clone());
                        }
                    }
                }
            }
            if out.len() == PageGroup::ALL.len() {
                break;
            }
        }
        out
    }

    /// Make the profile safe to use whatever a settings file says: unique page names, context
    /// entries that name existing pages.
    pub fn sanitize(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        // the next suffix to try per name: a suffix taken once stays taken, so each is tried once in all
        let mut next = std::collections::BTreeMap::<String, usize>::new();
        for (i, p) in self.pages.iter_mut().enumerate() {
            if p.name.trim().is_empty() {
                p.name = format!("Page {}", i + 1);
            }
            let base = p.name.clone();
            let mut n = next.get(&base).copied().unwrap_or(2);
            while !seen.insert(p.name.clone()) {
                p.name = format!("{base} {n}");
                n = n.saturating_add(1);
            }
            next.insert(base, n);
        }
        let names: std::collections::BTreeSet<String> = self.pages.iter().map(|p| p.name.clone()).collect();
        for list in self.contexts.values_mut() {
            list.retain(|n| names.contains(n));
        }
        self.contexts.retain(|_, l| !l.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::Binding as B;

    fn profile() -> Profile {
        let mut p = Profile {
            name: "T".into(),
            base: [("pad.1".to_string(), B::command("edit.undo"))].into_iter().collect(),
            pages: vec![
                Page::new("Cull", &[PageGroup::Keys], [("key.1", B::command("library.previous"))]),
                Page::new("Edit", &[PageGroup::Keys], [("key.1", B::command("develop.copy"))]),
                Page::new("Light", &[PageGroup::Dials], [("dial", B::value("light.exposure"))]),
                Page { auto: true, ..Page::new("Crop", &[], [("key.1", B::command("crop.rotateAspect")), ("dial", B::value("crop.angle"))]) },
            ],
            ..Default::default()
        };
        p.contexts.insert("default".into(), vec!["Cull".into(), "Light".into()]);
        p.contexts.insert("crop".into(), vec!["Crop".into()]);
        p
    }

    #[test]
    fn bindings_resolve_by_group_page_then_base() {
        let p = profile();
        let pages: BTreeMap<_, _> = [(PageGroup::Keys, "Edit".to_string()), (PageGroup::Dials, "Light".to_string())].into_iter().collect();
        assert_eq!(p.binding("key.1", &pages), Some(&B::command("develop.copy")));
        assert_eq!(p.binding("dial", &pages), Some(&B::value("light.exposure")));
        assert_eq!(p.binding("pad.1", &pages), Some(&B::command("edit.undo")), "base");
        assert_eq!(p.binding("key.2", &pages), None);
    }

    #[test]
    fn contexts_fall_back_to_parents_and_default() {
        let p = profile();
        let crop = p.pages_for_context("crop");
        assert_eq!(crop.get(&PageGroup::Keys).map(String::as_str), Some("Crop"));
        assert_eq!(crop.get(&PageGroup::Dials).map(String::as_str), Some("Crop"), "a page of both groups fills both");
        let d = p.pages_for_context("develop.light");
        assert_eq!(d.get(&PageGroup::Keys).map(String::as_str), Some("Cull"));
        assert_eq!(d.get(&PageGroup::Dials).map(String::as_str), Some("Light"));
    }

    #[test]
    fn paging_wraps_within_a_group() {
        let p = profile();
        assert_eq!(p.step_page(PageGroup::Keys, Some("Cull"), 1).as_deref(), Some("Edit"));
        assert_eq!(p.step_page(PageGroup::Keys, Some("Edit"), 1).as_deref(), Some("Cull"), "the automatic Crop page is skipped");
        assert_eq!(p.step_page(PageGroup::Keys, Some("Cull"), -1).as_deref(), Some("Edit"));
        assert_eq!(p.step_page(PageGroup::Keys, Some("Crop"), 1).as_deref(), Some("Edit"), "from an automatic page: on from the first");
        assert_eq!(p.step_page(PageGroup::Dials, None, 1).as_deref(), Some("Light"), "one dial page: stays");
        assert_eq!(Profile::default().step_page(PageGroup::Keys, None, 1), None);
    }

    #[test]
    fn sanitize_makes_names_unique_and_drops_dangling_contexts() {
        let mut p = profile();
        p.pages.push(Page { name: "Cull".into(), ..Default::default() });
        p.pages.push(Page::default());
        p.contexts.insert("x".into(), vec!["Missing".into()]);
        p.sanitize();
        let names: Vec<_> = p.pages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Cull", "Edit", "Light", "Crop", "Cull 2", "Page 6"]);
        assert!(!p.contexts.contains_key("x"));
    }
}
