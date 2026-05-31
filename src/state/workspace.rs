//! 複数 host を束ねる `Workspace` (issue #44 / 案 A)。
//!
//! ## 役割
//!
//! 単一 host 起動 (= `vozltop https://h/s`) と multi-host 起動 (= 複数 URL や
//! 複数 `@alias`) の双方を同じ runtime で扱えるようにする中間レイヤ。各 host は
//! 独立した `App` を持ち、UI から「今アクティブな host」だけが見える。
//!
//! ## 設計判断
//!
//! - **N=1 でも `Workspace`**: 単一 host 起動でも 1 entry の Workspace を作る。
//!   分岐コストより 1 タイプの runtime path を保つ方が読みやすい。
//! - **`App` は無改造**: 既存テスト (App / state mod 配下) を温存するため、App
//!   自体には触らない。Workspace は `App` を単に所有する。
//! - **Host 順序は起動時引数順**: argv 順 (= ユーザーが書いた順) を保つため
//!   `Vec<HostId>` + `HashMap<HostId, App>` の 2 段構成にする。`BTreeMap`
//!   ではアルファベット順になってしまい、`@prod @staging` と書いた意図が崩れる。
//! - **selected は index**: HostId を文字列で持ち回るより O(1) で active を引ける
//!   index の方が cycle 操作 (`next_host` / `prev_host`) も自然。
//!
//! ## 単一 host のときの UI
//!
//! `Workspace.multi() == false` のとき、UI は host タブバーを描画しない。これに
//! より既存 (issue #44 以前) のレンダ結果 / snapshot と完全に同一の出力になる。

use std::collections::HashMap;

use crate::state::App;

/// host を識別する文字列。
///
/// alias 経由なら alias 名 (`prod`), URL 直指定なら URL の `host:port`
/// 表現 (例: `nginx.example.com:8080`)。同名衝突は起動時に `Workspace::new`
/// で `(N)` サフィックスを付けて回避する。
pub type HostId = String;

/// 複数 host の `App` を束ねるルート。
///
/// `single_host(app)`: 単一 host の Workspace。host タブバーは出ない。
/// `new(entries)`: 複数 host。argv 順を保ち、初期 active は先頭 host。
#[derive(Debug)]
pub struct Workspace {
    /// host id → App。
    apps: HashMap<HostId, App>,
    /// argv に書かれた順を保つ host id 列。`active_index` の参照先でもある。
    order: Vec<HostId>,
    /// `order` の現在の active index (常に `order.len()` 未満)。
    active_index: usize,
}

impl Workspace {
    /// 単一 host の Workspace を作る。host id は固定で `"default"`。
    ///
    /// 単一 host runtime で使うほか、既存の App 単体テストを Workspace 経由の
    /// API でラップする用途にも使える。`multi()` は常に `false` を返すので、
    /// UI レンダ層は host タブバーを描画せず従来通りの出力になる。
    pub fn single_host(app: App) -> Self {
        let id = "default".to_string();
        let mut apps = HashMap::with_capacity(1);
        apps.insert(id.clone(), app);
        Self {
            apps,
            order: vec![id],
            active_index: 0,
        }
    }

    /// 複数 host の Workspace を作る。
    ///
    /// `entries` は `(host_id, app)` の Vec。`host_id` の重複は許容し、後勝ち
    /// ではなく `"<id>(2)"` 形式でサフィックスを付けて衝突回避する。argv 順を
    /// そのまま保つ。空 Vec は呼び出し側のバグなので panic する (Args::urls が
    /// `num_args = 1..` で 1 以上を保証する前提)。
    pub fn new(entries: Vec<(HostId, App)>) -> Self {
        assert!(
            !entries.is_empty(),
            "Workspace::new requires at least one host"
        );
        let mut apps = HashMap::with_capacity(entries.len());
        let mut order = Vec::with_capacity(entries.len());
        for (raw_id, app) in entries {
            let id = dedup_id(&apps, raw_id);
            order.push(id.clone());
            apps.insert(id, app);
        }
        Self {
            apps,
            order,
            active_index: 0,
        }
    }

    /// host 数。`single_host` で作った場合は 1。
    pub fn host_count(&self) -> usize {
        self.order.len()
    }

    /// host が 2 つ以上か (= UI で host タブバーを出すか)。
    pub fn multi(&self) -> bool {
        self.host_count() > 1
    }

    /// `order` 順の host id 列 (host タブバー描画用)。
    pub fn host_ids(&self) -> &[HostId] {
        &self.order
    }

    /// 現在の active host id。
    pub fn active_id(&self) -> &HostId {
        &self.order[self.active_index]
    }

    /// 現在の active host の index (0-based)。host タブバーのハイライト位置。
    pub fn active_index(&self) -> usize {
        self.active_index
    }

    /// active App への immutable 参照。
    pub fn active(&self) -> &App {
        &self.apps[&self.order[self.active_index]]
    }

    /// active App への mutable 参照 (`handle_key` が使う)。
    pub fn active_mut(&mut self) -> &mut App {
        let id = &self.order[self.active_index];
        self.apps.get_mut(id).expect("active id must exist in apps")
    }

    /// host id 指定で App を取り出す (fetch 結果のルーティング用)。
    pub fn app_mut(&mut self, id: &str) -> Option<&mut App> {
        self.apps.get_mut(id)
    }

    /// 次の host に切り替える (末尾なら先頭に戻る)。単一 host のときは no-op。
    pub fn next_host(&mut self) {
        if self.order.len() <= 1 {
            return;
        }
        self.active_index = (self.active_index + 1) % self.order.len();
    }

    /// 前の host に切り替える (先頭なら末尾に回る)。単一 host のときは no-op。
    pub fn prev_host(&mut self) {
        if self.order.len() <= 1 {
            return;
        }
        if self.active_index == 0 {
            self.active_index = self.order.len() - 1;
        } else {
            self.active_index -= 1;
        }
    }

    /// 全 host を `(id, &App)` で列挙する (アラート集計などで使う)。
    pub fn iter(&self) -> impl Iterator<Item = (&HostId, &App)> {
        self.order.iter().map(|id| (id, &self.apps[id]))
    }
}

/// 既存 id と衝突しない id を返す。衝突したら `"<base>(2)"`, `"<base>(3)"` … を試す。
fn dedup_id(existing: &HashMap<HostId, App>, base: HostId) -> HostId {
    if !existing.contains_key(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}({n})");
        if !existing.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_host_has_count_1_and_is_not_multi() {
        let ws = Workspace::single_host(App::new());
        assert_eq!(ws.host_count(), 1);
        assert!(!ws.multi());
        assert_eq!(ws.active_id(), "default");
        assert_eq!(ws.active_index(), 0);
    }

    #[test]
    fn new_preserves_argv_order() {
        let ws = Workspace::new(vec![
            ("prod".to_string(), App::new()),
            ("staging".to_string(), App::new()),
            ("dev".to_string(), App::new()),
        ]);
        assert_eq!(ws.host_count(), 3);
        assert!(ws.multi());
        assert_eq!(
            ws.host_ids(),
            &["prod".to_string(), "staging".to_string(), "dev".to_string()]
        );
        assert_eq!(ws.active_id(), "prod", "初期 active は先頭");
    }

    #[test]
    fn duplicate_ids_get_suffix() {
        let ws = Workspace::new(vec![
            ("api".to_string(), App::new()),
            ("api".to_string(), App::new()),
            ("api".to_string(), App::new()),
        ]);
        assert_eq!(
            ws.host_ids(),
            &[
                "api".to_string(),
                "api(2)".to_string(),
                "api(3)".to_string()
            ]
        );
    }

    #[test]
    fn next_host_cycles_and_wraps() {
        let mut ws = Workspace::new(vec![
            ("a".to_string(), App::new()),
            ("b".to_string(), App::new()),
            ("c".to_string(), App::new()),
        ]);
        assert_eq!(ws.active_id(), "a");
        ws.next_host();
        assert_eq!(ws.active_id(), "b");
        ws.next_host();
        assert_eq!(ws.active_id(), "c");
        ws.next_host();
        assert_eq!(ws.active_id(), "a", "末尾の次は先頭");
    }

    #[test]
    fn prev_host_cycles_in_reverse() {
        let mut ws = Workspace::new(vec![
            ("a".to_string(), App::new()),
            ("b".to_string(), App::new()),
            ("c".to_string(), App::new()),
        ]);
        ws.prev_host();
        assert_eq!(ws.active_id(), "c", "先頭の前は末尾");
        ws.prev_host();
        assert_eq!(ws.active_id(), "b");
    }

    #[test]
    fn next_prev_are_noop_on_single_host() {
        let mut ws = Workspace::single_host(App::new());
        ws.next_host();
        assert_eq!(ws.active_index(), 0);
        ws.prev_host();
        assert_eq!(ws.active_index(), 0);
    }

    #[test]
    fn active_mut_returns_active_app() {
        let mut ws = Workspace::new(vec![
            ("a".to_string(), App::new()),
            ("b".to_string(), App::new()),
        ]);
        ws.active_mut().cursor = 7;
        assert_eq!(ws.active().cursor, 7);
        ws.next_host();
        assert_eq!(ws.active().cursor, 0, "別 host は独立した cursor");
    }

    #[test]
    fn app_mut_by_id_routes_to_named_host() {
        let mut ws = Workspace::new(vec![
            ("a".to_string(), App::new()),
            ("b".to_string(), App::new()),
        ]);
        ws.app_mut("b").expect("b exists").cursor = 5;
        // active はまだ "a" だが b に書き込めている
        assert_eq!(ws.active_id(), "a");
        assert_eq!(ws.active().cursor, 0);
        ws.next_host();
        assert_eq!(ws.active().cursor, 5);
    }

    #[test]
    fn app_mut_unknown_id_returns_none() {
        let mut ws = Workspace::single_host(App::new());
        assert!(ws.app_mut("nonexistent").is_none());
    }

    #[test]
    #[should_panic(expected = "at least one host")]
    fn new_panics_on_empty() {
        // Args::urls は num_args = 1.. で 1 以上を保証するため、空 Vec で呼ばれる
        // のは呼び出し側のバグ。defensive panic で気付けるようにする。
        let _ = Workspace::new(Vec::new());
    }
}
