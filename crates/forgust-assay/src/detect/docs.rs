//! The documentation: the top-level `docs` folder (or `doc`, `documentation`) is a block.

use super::{Ctx, Draft};
use crate::model::{Kind, holds};

pub(super) fn detect(cx: &mut Ctx) {
    let Some(dir) = ["docs", "doc", "documentation"].into_iter().find(|d| cx.files.iter().any(|f| f.as_str() != *d && holds(d, f))) else {
        return;
    };
    let mut d = Draft::new(format!("docs:{dir}"), "Docs", Kind::Docs);
    d.roots.push(dir.to_string());
    cx.add(d);
}
