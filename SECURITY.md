# Security Policy

vozltop の脆弱性報告手順とサポート方針をまとめます。

## サポート対象バージョン

vozltop は単一バイナリとして配布される CLI ツールです。脆弱性修正は **latest minor (v1.x)** のみに対して提供します。pre-1.0 期間中は `main` ブランチを対象とします。

| バージョン | サポート |
|------------|----------|
| v1.x (latest minor) | ✅ |
| v1.x (older minor)  | ❌ |
| pre-1.0 (`main`)    | ✅ (best-effort) |

## 報告方法

脆弱性を発見した場合、**public な issue では報告しない**でください。代わりに以下のいずれかでお知らせください:

1. **GitHub Private Vulnerability Reporting (PVR)** — 推奨
   - リポジトリの [Security タブ](https://github.com/astail/vozltop/security/advisories/new) から "Report a vulnerability" を選択
   - GitHub アカウント上で非公開のやり取りが可能
2. **メール** — kiyomillefeuille@gmail.com
   - 件名に `[vozltop security]` を含めてください
   - 可能であれば PGP で暗号化（要望があれば公開鍵を別途提供）

## 報告に含めてほしい情報

- 影響を受けるバージョン (`vozltop --version` の出力)
- 再現手順（最小ケース）
- 想定される影響 (情報漏えい / DoS / RCE 等)
- 報告者の連絡先と公表名義 (希望があれば)

## 期待される対応

- **一次返信:** best-effort で **7 営業日以内**に受領確認
- **修正方針の連絡:** 受領後 14 営業日以内に「修正する/しない・想定タイムライン」を回答
- **修正リリース:** 重大度に応じ、合意したタイムラインで patch リリース

## 開示方針

[Coordinated Disclosure](https://en.wikipedia.org/wiki/Coordinated_vulnerability_disclosure) に従います:

1. 報告者と vozltop メンテナで修正タイムラインに合意
2. patch リリース後、GitHub Security Advisory として公開
3. 報告者の希望に応じてクレジットを記載 (デフォルト: 記載)

embargo 期間中の第三者開示はお控えください。

## 既知の制限

vozltop は read-only TUI として設計されており、`/status/control` 等の書き込み系エンドポイントは v1 では呼び出しません。次の項目はセキュリティ上の留意点として明記します:

- `--insecure` フラグは TLS 証明書検証を無効化します。本番環境での使用は非推奨です
- `--user user:pass` / `--header` で渡した認証情報は `argv` に残ります (詳細: issue #40)
- vozltop はリモートエンドポイントから受信した JSON を信頼します。攻撃者制御下の VTS エンドポイントを指す際はサイズ上限などに注意してください (詳細: issue #41)

これらの強化は v1 マイルストーンで継続対応中です。
