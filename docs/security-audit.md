---
type: Playbook
title: "依存関係セキュリティ監査"
description: "cargo-audit の実行手順 (Termux 含む) と advisory triage の記録。"
status: Current
tags:
  - security
  - dependencies
  - audit
verified:
  by: dsh
  at: "2026-11-16T23:15:44+09:00"
---

## 実行手順

初回のみ `cargo install cargo-audit --locked`。

```bash
cargo audit
cargo outdated --workspace
```

`.cargo/audit.toml` の ignore は根拠付きで管理する。

### Termux/Android の回避策

advisory DB の自動取得が `rustls-platform-verifier` の Android 初期化を
要求して失敗する (`Expect rustls-platform-verifier to be initialized`)。
system git で clone した DB を `--db` で指定する。

```bash
git clone --depth 1 https://github.com/RustSec/advisory-db.git \
  ~/.cache/rustsec-advisory-db
cargo audit --db ~/.cache/rustsec-advisory-db --no-fetch
```

## triage 記録 (2026-10-18)

対象: Cargo.lock の 468 crate。初回結果は vulnerabilities 5 / warnings 8。

### 対応済み

- `rustls` 0.23.45 → 0.23.46 (RUSTSEC-2026-0346〜0349、reqwest / oauth2
  経由)。
- `atty` 0.2.14 を opencode-tui から削除。未使用の直接依存だった
  (RUSTSEC-2024-0375 unmaintained / RUSTSEC-2021-0145 unsound)。

### ignore (.cargo/audit.toml)

- `rsa` 0.9.10 (RUSTSEC-2023-0071): 修正版なし (patched = [])。
  sqlx-mysql 経由で lock にのみ存在し build graph 外。

### 未対応 warning (記録のみ)

| crate | advisory | 依存元 | 解消条件 |
| --- | --- | --- | --- |
| `lru` 0.12.5 | RUSTSEC-2026-0002 / 0253 | ratatui 0.27 | ratatui 更新待ち |
| `bincode` 1.3.3 | RUSTSEC-2025-0141 | syntect 5.3.0 | syntect 更新待ち |
| `serial` 0.4.0 | RUSTSEC-2017-0008 | portable-pty 0.8 | portable-pty 更新待ち |
| `instant` 0.1.13 | RUSTSEC-2024-0384 | notify v7 | notify 更新待ち |
| `paste` 1.0.15 | RUSTSEC-2024-0436 | ratatui 0.27 | ratatui 更新待ち |

補足: `lru` の修正版は 0.16.3 以降のみで、ratatui 0.27 の `^0.12`
制約では適用できない。ratatui 更新時に再評価する。

## triage 記録 (2026-11-15)

対象: Cargo.lock の 541 crate。2026-11-14 の CI (workflows v1) で
新規 advisory 2 件により audit が失敗したため再トリアージした。

### 対応済み (2026-11-15)

- `pulldown-cmark` 0.13.5 → 0.13.6 (RUSTSEC-2026-0433、high 7.5、
  opencode-tui 経由)。

### ignore 追加

- `libsqlite3-sys` 0.37.0 (RUSTSEC-2026-0459 / CVE-2026-119375、medium 6.8):
  patched は `>= 0.39.0` だが、sqlx-sqlite 0.9.0 の依存制約が
  `libsqlite3-sys >=0.30.1, <0.38.0` で、sqlx 0.9.0 が最新 stable
  (2026-05-21 時点) のため互換な修正版を選択できない。攻撃条件は
  ローカルの攻撃者が細工した DB と `-wal` を置き、プロセスがそれを
  開くこと (AV:L / UI:P)。残余リスクとして受容し、sqlx 更新時に再評価する。

## triage 記録 (2026-11-16)

`cargo audit` を実行 (advisory DB 1509 件、Cargo.lock 544 crate)。
脆弱性 0 件、allowed warning 1 件 (unmaintained)。

| crate | advisory | 依存元 | 状況 |
| --- | --- | --- | --- |
| `bincode` 1.3.3 | RUSTSEC-2025-0141 | syntect 5.3.0 | 更新待ち |

`.cargo/audit.toml` の ignore は 2 件 (rsa / libsqlite3-sys) で変更なし。
2026-10-18 の表に記載した `lru` / `serial` / `instant` / `paste` は今回の
出力に含まれなかった。解消理由 (advisory DB 更新または依存バージョン変更)
は未確認。

### 依存更新 (2026-11-16)

`cargo outdated --workspace` で検出した 5 件を更新した。

- `notify` 8.2.1 → 8.2.2
- `toml` 1.1.8 → 1.2.1
- `serde_spanned` 1.1.2 → 1.1.3
- `toml_datetime` 1.1.2 → 1.1.3
- `toml_writer` 1.1.3 → 1.2.0

`Cargo.lock` では `windows-sys` 0.61.2 → 0.52.0 へのダウングレードが
5 箇所に入った (更新 dependency の制約によるものと推測、根拠未確認)。
Linux ビルドとテストには影響しない。Windows ターゲットのクロスチェックは
`x86_64-pc-windows-msvc` で試行したが、`ring 0.17.14` のビルドが MSVC の
`lib.exe` と Windows SDK ヘッダ (`assert.h`) 不在で失敗し、検証できなかった。
ダウングレードの Windows 側への影響は未確認のまま。

検証: `cargo check --all-targets`、`cargo clippy --workspace --all-targets
-- -D warnings`、`cargo test --workspace` (591 passed)、`cargo audit`、
`cargo outdated --workspace` (up to date) すべて成功。

## CI ゲート

共通ワークフロー `rust-gates.yml` の `audit: true` で有効化済み
(2026-10-18、workflows v1)。OpenCodeRust の `.github/workflows/ci.yml`
が opt-in している。

## 残作業

- sqlx が `libsqlite3-sys >=0.39` を許容したら RUSTSEC-2026-0459 の
  ignore を削除する。
- ratatui / syntect / portable-pty / notify 更新時に warning を再評価。
