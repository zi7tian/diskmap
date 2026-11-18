//! 直接序列化的树节点；避免为每个节点构造 BTreeMap 和重复分配字段名。

use std::borrow::Cow;

use serde::Serialize;

use crate::i18n::Text;
use crate::model::NCLS;

/// 字段声明按 JSON 键的字典序排列，直接输出与原先 Value/BTreeMap 保持相同顺序。
#[derive(Default, Serialize)]
pub(super) struct TreeNode<'a> {
    #[serde(rename = "c")]
    pub category: u8,
    #[serde(rename = "ch", skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TreeNode<'a>>>,
    #[serde(rename = "fl", skip_serializing_if = "Option::is_none")]
    pub flags: Option<u8>,
    #[serde(rename = "k", skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(rename = "m", skip_serializing_if = "Option::is_none")]
    pub more: Option<u8>,
    #[serde(rename = "n")]
    pub name: Cow<'a, str>,
    #[serde(rename = "nd", skip_serializing_if = "Option::is_none")]
    pub ndirs: Option<u64>,
    #[serde(rename = "nf", skip_serializing_if = "Option::is_none")]
    pub nfiles: Option<u64>,
    // 外层表示字段是否出现，内层保留“登记为空进程表时 p:null”的旧版语义。
    #[serde(rename = "p", skip_serializing_if = "Option::is_none")]
    pub processes: Option<Option<Text>>,
    #[serde(rename = "pk", skip_serializing_if = "Option::is_none")]
    pub package: Option<&'a str>,
    #[serde(rename = "r", skip_serializing_if = "Option::is_none")]
    pub running: Option<usize>,
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(rename = "sc", skip_serializing_if = "Option::is_none")]
    pub static_category: Option<u8>,
    #[serde(rename = "t")]
    pub kind: &'static str,
}

#[derive(Serialize)]
pub(super) struct TreeData<'a> {
    pub path: String,
    pub totals: [u64; NCLS],
    pub tree: TreeNode<'a>,
}

#[derive(Serialize)]
pub(super) struct HttpTree<'a, 'b> {
    pub path: &'b str,
    pub root: Cow<'a, str>,
    pub state: &'b str,
    pub totals: &'b [u64; NCLS],
    pub tree: &'b TreeNode<'a>,
}
