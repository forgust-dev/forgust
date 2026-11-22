//! A database's schema: a `migrations` folder (Rails: `db/migrate`) is a block, inside the
//! package that runs it, if any.

use super::{Ctx, Draft, dir_of, slug};
use crate::model::{Kind, Role};

pub(super) fn detect(cx: &mut Ctx) {
    let mut dirs: Vec<String> = Vec::new();
    for f in cx.files {
        let segs: Vec<&str> = f.split('/').collect();
        // folders only: the last segment is the file
        for i in 0..segs.len().saturating_sub(1) {
            if segs[i] == "migrations" || (segs[i] == "migrate" && i > 0 && segs[i - 1] == "db") {
                let dir = segs[..=i].join("/");
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
                break;
            }
        }
    }
    for dir in dirs {
        let parent = dir_of(&dir);
        let mut d = Draft::new(format!("migrations:{dir}"), &dir, Kind::Migrations);
        d.want_id = if parent == "." { "migrations".to_string() } else { format!("{}-migrations", slug(&cx.folder_name(&parent))) };
        d.role = Some(Role::Database);
        d.roots.push(dir);
        cx.add(d);
    }
}
