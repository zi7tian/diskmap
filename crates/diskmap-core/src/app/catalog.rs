//! 界面的只读索引查询。获取一次扫描句柄和进程快照，不改变扫描会话。

use std::path::PathBuf;

use crate::platform;
use crate::scan::Scan;
use crate::view::catalog::{self, GroupOptions, GroupPage, GroupQuery, Item, SearchResults};

use super::{App, lock};

fn query_path(scan: &Scan, path: &str) -> PathBuf {
    if path.is_empty() {
        scan.root().to_owned()
    } else {
        platform::norm_path(path)
    }
}

impl App {
    pub(crate) fn largest_files(&self, path: &str) -> Option<Vec<Item>> {
        let scan = self.current_scan()?;
        let path = query_path(&scan, path);
        let snapshot = self.inner.procs.snapshot();
        catalog::largest(&scan.index(), &snapshot, &path)
    }

    pub(crate) fn search_names(&self, path: &str, query: &str) -> Option<SearchResults> {
        let scan = self.current_scan()?;
        let path = query_path(&scan, path);
        let snapshot = self.inner.procs.snapshot();
        catalog::search(&scan.index(), &snapshot, &path, query)
    }

    pub(crate) fn group_page(
        &self,
        path: &str,
        parent: &str,
        options: GroupOptions,
    ) -> Option<GroupPage> {
        let session = lock(&self.inner.scan).clone()?;
        let scan = &session.scan;
        let path = query_path(scan, path);
        let parent = platform::norm_path(parent);
        let snapshot = self.inner.procs.snapshot();
        let finished = scan.state().is_finished();
        // 与 tree 使用相同的锁顺序、初始保留名单和目录投影缓存。
        let mut view = lock(&session.view);
        let index = scan.index();
        view.group_page(
            &index,
            &snapshot,
            GroupQuery {
                path: &path,
                parent: &parent,
                options,
            },
            finished,
        )
    }
}
