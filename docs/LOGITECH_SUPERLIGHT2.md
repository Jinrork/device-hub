# PRO X SUPERLIGHT 2 の移植

基点は Device HUB の `706203e837d48d29a479488d960b146524a522a9`。第1対象は PRO X SUPERLIGHT 2。有線と LIGHTSPEED を個別に検証する。G PRO Wireless は後続の対象。

## 上流との接続点

- `crates/device-hub/src/procmon.rs` は前面ウィンドウから exe 名を取得し、部分一致で `on_fg` と `on_leave` を適用する。現行ルールを維持しつつ、Logicool 用は実行ファイルのフルパスで現在必要なプロファイルを決める。切断や復帰をまたいで過去の設定変更を順に再送すると古いアプリの設定が発火し得るため、Logicool 用は最新の選択と接続世代だけを保持する。
- `crates/devhub-hid/src/lib.rs` は registry とドライバを結び、デバイスごとのセッションを生成する。`transport.rs` の番号付きレポート経路は移植候補。今回追加した `logitech_hidpp.rs` は IRoot の機能照会パケット構築と応答分類までを担当する。実 HID への接続、registry の機種追加、設定の書き込みは未実装。
- 上流の GUI はウィンドウ寿命とバックグラウンド処理が結び付いている。実用版はログイン中の単一プロセス、ウィンドウを閉じても続く通知領域、明示的な終了を目標にする。自動起動はユーザー単位の Startup に専用ショートカット1個を置き、削除で解除する。サービスやドライバを必要としない経路を優先する。試作用の起動登録は行っていない。

## HID++ 調査

| 項目 | 公開資料・今回の確認 |
| --- | --- |
| SUPERLIGHT 2 有線 | `046D:C09B`。公開例の HID++ 直結宛先は `0xFF`。今回の Windows セッションでは未確認 |
| LIGHTSPEED | `046D:C54D` と `FF00/0001`・`FF00/0002` の HID コレクションを Windows の Raw Input で確認。公開例の内側は WPID `40A9`・スロット 1・HID++ 4.2。ペアリング先とスロットを照会で確認する |
| 機能探索 | `0x0000` IRoot の GetFeature で、機能 ID に対するインデックス・フラグ・バージョンを取得。固定インデックスに依存しない。7 バイト／20 バイトの応答を分類し、対象・機能・関数・Software ID を照合する |
| 電池 / DPI / レート | 公開ログで `1004` / `2202` / `8061` を確認。転送経路、ファームウェア、モードごとの設定可否は未検証 |
| ボタン | 公開ログで `8110` Mouse Button Spy が見え、`1B04` Reprogrammable Controls V4 は現れない。別機種の `8110` マッピング実測を手掛かりに、先に標準サイドボタンの挙動と `8110` の通知・照会を検証する。他機種と同じ機能番号でも再マップや揮発性は未確定 |
| オンボード | `8100` の存在とレイアウト `0x06` の公開報告あり。モード変更で結果が変わる報告があり、永続領域を変更せずに設定できるかは個別確認が必要 |

根拠 — [Logitech HID++ 仕様](https://github.com/Logitech/cpg-docs/tree/master/hidpp20) · [libratbag の SUPERLIGHT 2 実測](https://github.com/libratbag/libratbag/issues/1519) · [Solaar の SUPERLIGHT 2 ログ](https://github.com/pwr-Solaar/Solaar/issues/2207) · [別機種での 0x8110 実測](https://github.com/libratbag/libratbag/issues/1831) · [OpenMouse のオンボード解析](https://github.com/OpenMouse-Project/mouse-protocol/blob/main/docs/logitech-onboard-profiles.md)

将来の通信は接続ごとに一問一答とし、応答待ちの期限を設ける。Windows では短いレポートと長いレポートが別コレクションに現れるため、物理的に同じ受信機に属することを確認し、送信先と応答の経路を実測する。ホスト側の Software ID で他アプリの応答を避けられても、同じ設定の競合は防げない。タイムアウト、動作中の抜き差し、スリープ復帰で該当セッションを無効化し、再列挙と機器同定から再開する。前の接続を待っていた処理は再送しない。

## Windows 診断版

`crates/device-hub/examples/logitech_monitor.rs` は Windows API を直接使用し、250ms ごとに実際の前面アプリを取得する。未登録・取得不能なら Desktop、休止中は Paused。復帰通知で世代を進め、前面が同じ場合も再評価する。画面に出るのはプロファイル名と列挙モードの候補 ID だけ。デバイスハンドル、入力フック、設定の自動適用を使わない。

```powershell
rustc +stable --edition 2021 crates/device-hub/examples/logitech_monitor.rs -o .\logitech_monitor.exe
.\logitech_monitor.exe --inventory
.\logitech_monitor.exe .\profiles.tsv
```

UTF-8 の `profiles.tsv` は1行につき「プロファイル名、タブ、exe の絶対パス」。未登録なら `Desktop`、終了は Ctrl+C。起動だけ確認するには末尾に `--once` を付ける。自動切替と復帰の設計は [Windows の電源通知 API](https://learn.microsoft.com/en-us/windows/desktop/api/powerbase/nf-powerbase-powerregistersuspendresumenotification) と [Raw Input 列挙 API](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getrawinputdevicelist) を参照。

## 受入までの順序

1. 診断版で前面切替、同名の別アプリ、スリープ／復帰と抜き差しを確認する。G HUB を通常どおり稼働させ、Windows の標準ボタン動作を基準に記録する。
2. 対象機器、送信する Get の一覧、試験条件を提示し、承認後に有線と無線それぞれで、ペアリング先・ファームウェア・実装機能を確認する。Get もホストからレポートを送る。タイムアウト、競合、未知の応答で中断する。
3. まずサイドボタンの割当を試す。マウスを機器単位で識別し、二重入力、押しっぱなしでの切替や復帰、他マウスへの影響を検証する。デバイス側変更が必要なら、揮発性と復元手順を確認した操作のみ個別承認を受ける。ファームウェア更新、ペアリング変更、オンボード領域への書き込みは移植経路に入れない。
4. G HUB の切替はアンインストールを前提にせず、通常終了と再起動で戻せるセッションから確認する。実際のゲーム・アプリで切替、割当、再ログイン、復帰を確認する。各接続方式で復帰10回を行い、G HUB Agent 再起動の有無、最初の有効入力までの時間、欠落・二重発火を記録する。条件を満たしてからユーザー単位のログイン起動を有効化する。
