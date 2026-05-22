//! CLI 引数の保持とパース時バリデーション。
//!
//! 本 PR (issue #18) では認証フラグを raw 文字列として `Args` に持たせる:
//!
//! - `--user user:pass`
//! - `--header 'K: V'` (繰り返し可)
//! - `--insecure`
//!
//! `Args` は raw 文字列を保持し、`parse_user` / `parse_header` の検証関数を
//! 通って初めて reqwest 用の型に正規化される。これは:
//!
//! 1. issue #34 で `clap::Parser` を入れる際、`value_parser = parse_header` と
//!    指定するだけで CLI パース時バリデーションが完成する形にしておくため
//! 2. 「argv に平文 password」問題 (issue #40) で `VOZLTOP_PASSWORD` 環境変数
//!    フォールバックを足すとき、`Args` 側に押し込まず `parse_user` の呼び出し
//!    側 (= `VtsClient::new`) に責務を寄せたいため

use std::str::FromStr;

use color_eyre::eyre::{eyre, Result, WrapErr};
use reqwest::header::{HeaderName, HeaderValue};
use url::Url;

/// 後続 issue で拡張される CLI 引数。
#[derive(Debug, Clone)]
pub struct Args {
    /// nginx-vts の `/status/format/json` などを指す絶対 URL。
    pub url: Url,
    /// HTTP Basic 認証用の `user:pass`。バリデーションは `parse_user` で行う。
    pub user: Option<String>,
    /// 追加ヘッダの raw 文字列 (`K: V`)。バリデーションは `parse_header` で行う。
    pub headers: Vec<String>,
    /// TLS 証明書検証を無効化する。`true` の場合 `VtsClient::new` で stderr に
    /// 警告を出す。
    pub insecure: bool,
}

/// `user:pass` 形式を `(user, password)` に分解する。
///
/// - 最初のコロンで 1 回だけ split (RFC 7617 では password 側のコロンは許容)
/// - username が空はエラー (RFC 7617 で禁止)
/// - password が空は OK (curl の挙動に合わせる)
pub fn parse_user(s: &str) -> Result<(String, String)> {
    let (user, pass) = s
        .split_once(':')
        .ok_or_else(|| eyre!("--user must be in `user:pass` format (got {s:?})"))?;
    if user.is_empty() {
        return Err(eyre!("--user: username must not be empty (got {s:?})"));
    }
    Ok((user.to_string(), pass.to_string()))
}

/// `K: V` 形式を `(HeaderName, HeaderValue)` に分解する。
///
/// - 最初のコロンで 1 回だけ split (value 側のコロンは許容: `Bearer xyz:abc` など)
/// - name の前後の空白は trim する
/// - value の前後の OWS (= 0 個以上の SP/HTAB) を RFC 9110 §5.5 に従って除去する
/// - `HeaderName::from_str` / `HeaderValue::from_str` の検証に委譲
///   (CR/LF などのヘッダインジェクションは reqwest 側で弾かれる)
pub fn parse_header(s: &str) -> Result<(HeaderName, HeaderValue)> {
    let (name, value) = s
        .split_once(':')
        .ok_or_else(|| eyre!("--header must be in `K: V` format (got {s:?})"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(eyre!("--header: name must not be empty (got {s:?})"));
    }
    // RFC 9110 §5.5: OWS = *( SP / HTAB ). value の前後の OWS は意味を持たない。
    let value = value.trim_matches(|c: char| c == ' ' || c == '\t');
    let name =
        HeaderName::from_str(name).wrap_err_with(|| format!("--header: invalid name {name:?}"))?;
    let value = HeaderValue::from_str(value)
        .wrap_err_with(|| format!("--header: invalid value {value:?}"))?;
    Ok((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_accepts_basic_form() {
        let (u, p) = parse_user("alice:secret").unwrap();
        assert_eq!(u, "alice");
        assert_eq!(p, "secret");
    }

    #[test]
    fn parse_user_allows_colon_in_password() {
        // RFC 7617: username は colon 禁止だが password は colon 含んで OK
        let (u, p) = parse_user("alice:s:e:c:r:e:t").unwrap();
        assert_eq!(u, "alice");
        assert_eq!(p, "s:e:c:r:e:t");
    }

    #[test]
    fn parse_user_allows_empty_password() {
        let (u, p) = parse_user("alice:").unwrap();
        assert_eq!(u, "alice");
        assert_eq!(p, "");
    }

    #[test]
    fn parse_user_rejects_no_colon() {
        assert!(parse_user("alice").is_err());
    }

    #[test]
    fn parse_user_rejects_empty_username() {
        assert!(parse_user(":secret").is_err());
        assert!(parse_user("").is_err());
    }

    #[test]
    fn parse_header_accepts_basic_form() {
        let (n, v) = parse_header("Authorization: Bearer xyz").unwrap();
        assert_eq!(n.as_str(), "authorization");
        assert_eq!(v.to_str().unwrap(), "Bearer xyz");
    }

    #[test]
    fn parse_header_allows_colon_in_value() {
        let (_, v) = parse_header("X-Token: foo:bar:baz").unwrap();
        assert_eq!(v.to_str().unwrap(), "foo:bar:baz");
    }

    #[test]
    fn parse_header_accepts_no_space_after_colon() {
        let (_, v) = parse_header("X-Foo:bar").unwrap();
        assert_eq!(v.to_str().unwrap(), "bar");
    }

    #[test]
    fn parse_header_rejects_no_colon() {
        assert!(parse_header("invalid").is_err());
    }

    #[test]
    fn parse_header_rejects_empty_name() {
        assert!(parse_header(": only-value").is_err());
        assert!(parse_header(":").is_err());
    }

    #[test]
    fn parse_header_rejects_invalid_name() {
        // スペース入りの header name は HTTP 仕様違反
        assert!(parse_header("Bad Name: value").is_err());
    }

    #[test]
    fn parse_header_rejects_crlf_injection() {
        // CR/LF を含む値は reqwest::header::HeaderValue 側で弾かれる
        assert!(parse_header("X-Foo: bar\r\nEvil: yes").is_err());
    }

    #[test]
    fn parse_header_trims_multiple_leading_and_trailing_ows() {
        // RFC 9110 OWS 除去: SP/HTAB が複数あっても全て削る
        let (_, v) = parse_header("X-Foo:   bar  ").unwrap();
        assert_eq!(v.to_str().unwrap(), "bar");
        let (_, v) = parse_header("X-Foo:\t\t bar\t").unwrap();
        assert_eq!(v.to_str().unwrap(), "bar");
    }
}
