//! 优化前的渲染器，只用于逐项验证输出一致；保持原来的全排序和逐文件路径查询。

use super::*;

struct Node {
    value: Value,
    children: Option<Vec<usize>>,
}

pub(super) fn render(
    index: &Index,
    id: DirId,
    snapshot: &Snapshot,
    retained: &Snapshot,
    options: TreeOptions,
    pkgdb: Option<&PkgDb>,
) -> Value {
    let min_size = ((index.dir(id).size() as f64 * options.frac) as u64).max(1);
    let mut budget = options.max_nodes;
    let mut nodes = vec![Node {
        value: directory(index.dir(id), snapshot),
        children: None,
    }];
    let mut queue = VecDeque::from([(id, 0, 0)]);
    while let Some((id, output, depth)) = queue.pop_front() {
        let dir = index.dir(id);
        if depth >= options.max_depth || budget == 0 {
            if dir.subdirs().len() > 0 || dir.files().next().is_some() {
                nodes[output].value["m"] = json!(1);
            }
            continue;
        }
        let mut children = Vec::new();
        let mut small = Aggregate::default();
        for child in dir.subdirs() {
            if child.size() >= min_size {
                let child_output = push_node(&mut nodes, directory(child, snapshot));
                children.push(child_output);
                queue.push_back((child.id(), child_output, depth + 1));
            } else {
                small.add(child.size(), 1, child.category());
            }
        }
        let mut files: Vec<_> = dir.files().collect();
        let compact = files.len() > KEEP_FILES;
        if compact {
            files.sort_by_key(|file| std::cmp::Reverse(file.size()));
        }
        let mut rest = Aggregate::default();
        let parent_path = dir.path();
        for (position, file) in files.into_iter().enumerate() {
            let path = join(index, &parent_path, file.name());
            let running = snapshot.lookup(&path).is_some();
            if compact && position >= KEEP_FILES && retained.lookup(&path).is_none() {
                rest.add(file.size(), 1, file.category());
            } else if file.size() < min_size {
                small.add(file.size(), 1, file.category());
            } else {
                let mut value = json!({
                    "n": file.name().to_string_lossy(), "s": file.size(),
                    "c": file.category() as u8, "t": "f",
                });
                if running {
                    value["sc"] = json!(file.category() as u8);
                    value["c"] = json!(Category::Running as u8);
                    value["p"] = json!(snapshot.summary(&path));
                }
                if let Some(owner) = pkgdb.and_then(|db| db.owner(&path))
                    && !owner.is_empty()
                {
                    value["pk"] = json!(owner);
                }
                children.push(push_node(&mut nodes, value));
            }
        }
        if rest.count > 0 {
            if rest.size >= min_size {
                children.push(push_node(&mut nodes, rest.value()));
            } else {
                // 旧版先聚合，再参与面积阈值：类别票数是聚合块大小 + 1，而不是逐文件的票数。
                small.add(rest.size, rest.count, rest.category());
            }
        }
        if small.size > 0 || small.count > 0 {
            children.push(push_node(&mut nodes, small.value()));
        }
        children.sort_by_key(|&child| {
            std::cmp::Reverse(nodes[child].value["s"].as_u64().unwrap_or_default())
        });
        budget = budget.saturating_sub(children.len());
        nodes[output].children = Some(children);
    }
    // 广度优先控制预算，倒序组装 JSON；不在索引上递归，避免深目录导致栈溢出。
    for i in (0..nodes.len()).rev() {
        if let Some(children) = nodes[i].children.take() {
            let values = children
                .into_iter()
                .map(|child| std::mem::take(&mut nodes[child].value))
                .collect();
            nodes[i].value["ch"] = Value::Array(values);
        }
    }
    std::mem::take(&mut nodes[0].value)
}

fn push_node(nodes: &mut Vec<Node>, value: Value) -> usize {
    let id = nodes.len();
    nodes.push(Node {
        value,
        children: None,
    });
    id
}

fn directory(dir: Dir<'_>, snapshot: &Snapshot) -> Value {
    let mut value = json!({
        "n": dir.name().to_string_lossy(), "s": dir.size(), "c": dir.category() as u8,
        "t": "d", "nf": dir.nfiles(), "nd": dir.ndirs(),
    });
    if dir.flags() != 0 {
        value["fl"] = json!(dir.flags());
    }
    let count = snapshot.dir_count(dir.path());
    if count != 0 {
        value["r"] = json!(count);
    }
    value
}

fn join(index: &Index, parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let mut path = parent.as_os_str().to_os_string();
    let bytes = path.as_encoded_bytes();
    if !bytes
        .last()
        .is_some_and(|&b| b == b'/' || (index.os() == crate::platform::Os::Windows && b == b'\\'))
    {
        path.push(index.os().sep().to_string());
    }
    path.push(name);
    PathBuf::from(path)
}
