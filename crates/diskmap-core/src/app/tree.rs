//! 每次扫描独占的序列化树响应 LRU；扫描中不缓存，避免依赖公共扫描模块的发布版本。

use std::collections::VecDeque;

use super::*;
use crate::i18n::{self, Lang};

const MAX_ENTRIES: usize = 8;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(PartialEq, Eq)]
struct Key {
    path: Vec<u8>,
    frac: u64,
    lang: Lang,
}

#[derive(Clone)]
struct Version {
    state: ScanState,
    processes: Arc<procinfo::Snapshot>,
    components: Arc<Components>,
    settings: u64,
}

impl Version {
    fn same(&self, other: &Self) -> bool {
        self.state == other.state
            && Arc::ptr_eq(&self.processes, &other.processes)
            && Arc::ptr_eq(&self.components, &other.components)
            && self.settings == other.settings
    }
}

#[derive(Default)]
pub(super) struct TreeResponses {
    version: Option<Version>,
    entries: VecDeque<(Key, Arc<[u8]>)>,
    bytes: usize,
}

impl TreeResponses {
    fn get(&mut self, version: Version, key: &Key) -> Option<Arc<[u8]>> {
        if !version.state.is_finished()
            || !self.version.as_ref().is_some_and(|old| old.same(&version))
        {
            self.entries.clear();
            self.bytes = 0;
        }
        self.version = Some(version);
        if let Some(position) = self.entries.iter().position(|(old, _)| old == key) {
            let entry = self.entries.remove(position).expect("entry exists");
            let bytes = Arc::clone(&entry.1);
            self.entries.push_back(entry);
            return Some(bytes);
        }
        None
    }

    fn insert(&mut self, version: &Version, key: Key, bytes: Arc<[u8]>) {
        if version.state.is_finished()
            && self
                .version
                .as_ref()
                .is_some_and(|current| current.same(version))
            && bytes.len() <= MAX_BYTES
        {
            // 并发的相同请求只保留一条；旧版本的慢请求不能在失效后重新填回缓存。
            if let Some(position) = self.entries.iter().position(|(old, _)| *old == key) {
                let (_, removed) = self.entries.remove(position).expect("entry exists");
                self.bytes -= removed.len();
            }
            while self.entries.len() >= MAX_ENTRIES || self.bytes + bytes.len() > MAX_BYTES {
                let (_, removed) = self.entries.pop_front().expect("cache is not empty");
                self.bytes -= removed.len();
            }
            self.bytes += bytes.len();
            self.entries.push_back((key, Arc::clone(&bytes)));
        }
    }

    #[cfg(test)]
    fn get_or_render(
        &mut self,
        version: Version,
        key: Key,
        render: impl FnOnce() -> Option<Arc<[u8]>>,
    ) -> Option<Arc<[u8]>> {
        if let Some(hit) = self.get(version.clone(), &key) {
            return Some(hit);
        }
        let bytes = render()?;
        self.insert(&version, key, bytes.clone());
        Some(bytes)
    }
}

impl App {
    /// 返回完整 `/api/tree` JSON 字节。命中时只克隆 Arc，不克隆 Value 或复制响应正文。
    /// 新扫描有新缓存；进程快照、分类组件、设置版本、结束状态变化时清空缓存。
    /// 扫描中每次读取当前索引且不保存响应，新目录发布不可能命中旧字节。
    pub fn tree_bytes(&self, path: &str, frac: f64) -> Option<Arc<[u8]>> {
        let session = lock(&self.inner.scan).clone()?;
        let path = if path.is_empty() {
            session.scan.root().to_owned()
        } else {
            platform::norm_path(path)
        };
        let state = session.scan.state();
        let processes = self.inner.procs.snapshot();
        let components = self.components();
        let lang = i18n::current();
        let _scope = i18n::scope(lang);
        let version = Version {
            state,
            processes: Arc::clone(&processes),
            components: Arc::clone(&components),
            settings: self.inner.settings_version.load(Ordering::Acquire),
        };
        let key = Key {
            path: platform::path::path_key(Os::current(), path.as_os_str().as_encoded_bytes()),
            frac: frac.to_bits(),
            lang,
        };
        if let Some(hit) = lock(&session.responses).get(version.clone(), &key) {
            return Some(hit);
        }
        // 不持有响应缓存锁渲染；另一已缓存路径的命中不等待本次冷请求。
        let bytes: Arc<[u8]> = {
            let mut view = lock(&session.view);
            let index = session.scan.index();
            view.tree_bytes(
                &index,
                processes,
                &path,
                frac,
                Some(&components.pkgdb),
                state.key(),
            )?
            .into()
        };
        lock(&session.responses).insert(&version, key, Arc::clone(&bytes));
        Some(bytes)
    }
}

#[cfg(test)]
mod tests;
