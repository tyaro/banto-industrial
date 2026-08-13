# banto-rtsp 基礎クレート設計

作成日: 2026-08-13
状態: Draft（Phase 1コード/ローカルcapability・上位レビュー完了、実機/配布確認待ち）
最終検証日: 2026-08-14

## 位置づけ

`banto-rtsp` は、public な `banto-industrial` で再利用できる RTSP 共通層の
第一スライスである。アプリケーション固有の Tauri UI や Banto-HUB 接続を
持たず、値・エラー・JPEG処理とFFmpeg process/supervisor ownershipを提供する。
`codex/banto-rtsp-foundation`上のPhase 1実装は上位レビューと独立検証まで完了しているが、
現時点では未コミット・未プッシュである。

## 初版の非スコープ

- FFmpeg sidecarの最終配布version/license判断と実機RTSP接続試験
- crate自身によるRTSP socket/protocol stack（接続はFFmpeg childへ委譲）
- Tauri IPC、Svelte UI、axum/HTTP
- Banto-HUB や Modbus への接続
- JPEG 以外の映像形式、音声、映像中継
- 実機 URL、IP アドレス、ユーザー名、パスワード

## 第二スライス: FFmpeg launch 準備

`FfmpegInputFile` は呼び出し側が指定した `.ffconcat` exact path を `create_new` で開き、
既存ファイルを上書きしない。`RtspEndpoint`、`RtspCredentials`、transportから、version、
認証付きURL一件の`file` directive、`option rtsp_transport tcp|udp`、必須の
`option timeout <microseconds>`を持つ短命ffconcat manifestを作る。URLはFFmpegの
single-quote/backslash規則でescapeし、argvにはmanifest pathだけを渡す。argv は `OsString`
の配列として保持し、shell の文字列連結は行わない。入力
ファイルのguardは自身が作成したexact fileだけをDrop時に削除し、親directoryや再帰的な
削除は行わない。

userinfo は UTF-8 byte 単位で percent encode し、unreserved 文字だけを素通しする。
`FfmpegCommandSpec`はFFmpeg executable、`-f concat -safe 0`、local manifestとRTSP(S)の
TCP/UDP/RTP/TLSに限定したexplicit protocol whitelist、通常の`-i <manifest>`、`-nostdin`、
MJPEG `image2pipe` stdoutの最小argvを構成する。ローカルFFmpeg 9.0.1 essentialsで
slash-prefixed input optionを実測したところ`Unrecognized option '/i'`となったため、この方式は
不採用とした。[FFmpeg公式ffmpeg-all](https://ffmpeg.org/ffmpeg-all.html)のconcat demuxerは
`file path`と`option key value` directiveを明示しており、一件のlocal mediaを参照するoffline
smokeでは同じconcat argvからMJPEG生成に成功した。

FFmpeg 9.0.1のRTSP demuxerでは`timeout`の既定値が0（無期限）であり、接続先がaccept後に
応答しない場合、childが終了せずsupervisorのrestartへ進めない。`RtspConfig`はfiniteな
`io_timeout: Duration`を必須とし、0、1 microsecond未満、FFmpegのsigned 64-bit
microsecondsへ変換できない値を構造化Config errorで拒否する。検証済み値だけをmanifestの
`option timeout`へ整数として書くため、文字列注入やDuration変換の切り捨て・overflowを許さない。

FFmpeg stderr は `FfmpegLogSanitizer` を通してからログ、UI、status へ渡す。認証 URL、
raw/percent-encoded の username/password を `[REDACTED]` に置換し、空文字列を
置換パターンにはしない。ファイル作成・書込み・削除のエラーは元の URL や
資格情報を保持せず、ErrorKind と安定した分類/code だけを返す。

Windows ではファイル ACL をこの crate が設定しないため、呼び出し側が保護済みの
runtime directory を用意し、他ユーザーから読めない ACL を担保する。Unix では
作成時に mode `0600` を設定する。guard は FFmpeg がmanifestを読み終える
まで必要だが、production supervisorは最初のframeをpublishした直後にcleanupする。
first-frame status通知はcleanup成功後に行い、launch timeout/error、正常終了、異常終了、
起動失敗を含む全終了経路でもguardのDropによりcleanupする。

この方式でも秘密が完全に残らないわけではない。認証 URL は短時間ファイルと
プロセスメモリに存在し、FFmpeg や OS の内部状態、クラッシュダンプ、バックアップ
等に残る可能性がある。ファイル cleanup、runtime directory ACL、stderr sanitization、
ログ保持方針を上位アプリケーションでも継続して管理する。

## 第三スライス: child ownership と latest-frame store

`FfmpegChild` は `FfmpegCommandSpec` の executable と `OsString` argv を
`std::process::Command` へ個別に渡して直接起動する。shell 文字列連結、`cmd /C`、
PowerShell は使わない。stdin は null、stdout/stderr は pipe とし、各ストリームは
take-once API で所有権を移す。二度目の取得は秘密を含まない構造化 Launch エラーに
なる。Windows では `CREATE_NO_WINDOW` を設定して GUI 起動時の sidecar console
flash を抑止し、Unix では標準の child process ownership に従う。

`try_wait`、`wait`、`terminate` は終了状態をキャッシュし、既に終了・reap 済みの
child を冪等に扱う。`terminate` は kill の成否にかかわらず wait を試行し、wait が
成功した場合は終了状態を保存する。Drop は子が稼働中または状態確認に失敗した場合に
kill と wait を best effort で行うが、recursive process kill はしない。このスライスは
stdout reader、stderr sanitizer pump、JPEG decoder 接続、restart loop、Tokio、Tauri
を実装しない。

`LatestFrameStore` は最新の `Arc<VideoFrame>` 一枚だけを保持し、publish ごとに
sequence を割り当てて古い値を置換する。`LatestFrameHandle` は snapshot と
`Condvar` による `wait_for_newer` だけを公開する read-only consumer handle であり、
publish/close 権限は持たない。close は全 waiter を起こし、close 後 publish は
structured error とする。sequence は `u64::MAX` の次で wrap せず、上限到達後の publish
を `SequenceExhausted` として拒否する。Mutex poisoning は panic/unwrap で処理せず、
構造化エラーに変換する。Debug は状態と sequence のみを出力し、JPEG payload は出さない。

プロセスの単体テストは shell に依存せず current test executable を直接起動する。
その実行ファイルは即時終了する可能性があり、Drop テストは active な長寿命 child を
kill したことまでは実証しない。この残余リスクは、将来 supervisor の統合テストで
プラットフォーム別に active child の終了、reap、アプリ終了時の残留プロセスなしを
確認して解消する。

## 第四Aスライス: FFmpegログのストリーミング無害化

FFmpegの標準エラー出力は任意のbyte chunk境界で届くため、chunk単位の独立した
文字列置換だけでは、境界をまたぐendpointや資格情報を取りこぼす。第四Aでは
`FfmpegLogStreamSanitizer`を追加し、登録済み秘密patternの最大長から1byte短い
carryを保持して、1byteずつ入力されても秘密を出力しない契約を定める。

- endpoint自体は資格情報の有無にかかわらず常時秘密patternとして扱う。
- raw認証URL、percent-encoded認証URL、raw/upper/lower encoded資格情報を除去する。
- invalid UTF-8を含むFFmpeg出力はbyte列のまま安全に処理する。
- `finish`で残存carryを無害化して返し、Debugにはpatternや資格情報を出さない。
- 既存の一括`sanitize`は互換性を維持する。

## 第四Bスライス: bounded diagnostics

`FfmpegDiagnostics`はストリーミングsanitizerを通過済みのFFmpeg診断文字列だけを
保持するproducerである。raw stderrを直接渡してはならない。保持件数とentryごとの
byte上限をconstructorで固定し、`VecDeque`が上限へ達した場合は最古のentryを破棄する。
文字列の切り詰めはUTF-8境界を守り、上限内に収まる場合だけ末尾へ
`[truncated]`を付ける。

`FfmpegDiagnosticsHandle`は同じstoreを`Arc`で共有するread-only consumerであり、
公開操作は`Vec<String>`を返す`snapshot`だけとする。producerだけが
`push_sanitized`と冪等な`close`を持ち、close後のpushは構造化された`Closed`エラーに
する。同期状態のpoisonはpanicせず`Poisoned`へ変換する。双方のDebugは設定値、
entry件数、closed/poisonedだけを出し、診断本文を含めない。

## 第四Cスライス: generic reader pumps

`pump_jpeg_stream`はgenericな`Read`から固定長stack bufferでFFmpeg stdoutを読み、
既存`JpegFrameDecoder`が返した全frameを`LatestFrameStore`へpublishする。最初のframeを
publishした直後に短命input fileを明示cleanupし、それ以前のEOF、read失敗、decoder失敗、
store失敗ではguardのDrop cleanupへ委ねる。decoder/storeの既存構造化エラーは分類を
維持し、reader I/Oだけを`PumpError`へ分類する。

`pump_stderr`はgenericな`Read`のbyte列を必ず`FfmpegLogStreamSanitizer`へ通し、
sanitizedな非空chunkだけをlossy UTF-8変換して`FfmpegDiagnostics`へ保存する。raw stderrを
diagnostics、error、Debugへ渡さず、改行まで蓄積するunbounded bufferも持たない。EOFでは
必ずsanitizerをfinishする。read失敗時にもsanitized carryを保存してからstructured read
errorを返すが、その保存自体が失敗した場合は、安全化済みtailを保持できなかったことを
示すdiagnostics errorを優先する。

両pumpはbyte数、publish frame数、first-frame有無だけの`PumpSummary`を返す。この
スライスはreader thread、session ownership、複数pumpのjoin、child restart supervisorを
実装しない。これらは次スライスで扱う。

## 第四Dスライス: one-shot FFmpeg session

`FfmpegSession`は直接起動した`FfmpegChild`一つと、`banto-rtsp-stdout`、
`banto-rtsp-stderr`という名前を固定した二つの`std::thread` workerを所有する。
stdout/stderr pipeをそれぞれ一度だけtakeし、短命input guardはstdout pumpへ移動する。
両workerを起動してからchildをwait/reapし、結果にかかわらず両workerをjoinしてframe storeと
diagnosticsをcloseする。threadの部分起動に失敗した場合はchildをterminate/reapして、開始済み
workerをjoinする。workerをdetachせず、childを意図的にzombieとして残さない。

公開`FfmpegSession::run`はone-shot所有権として、成功・失敗を問わずworker join後に
frame storeとdiagnosticsをcloseする。一方、将来のrestart supervisorはconsumer handleを
再接続のたびに作り直さず安定して公開する必要があるため、crate内部に限って両storeを
closeしない実行経路を持つ。このinternal経路でもchildのterminate/reapとworker joinは
同一であり、storeの生存期間だけをsupervisor所有へ移す。公開APIへclose忘れを起こす
policy指定は露出しない。

実行時のエラー優先順位は、child wait/reap、stdout panic、stderr panic、stdout pump、
stderr pump、frame-store close、diagnostics close、non-success exitの順とする。setup中の
pipe取得/thread spawn失敗はsetup errorを優先するが、終了・reap・開始済みjoinは必ず試みる。
non-success exitは安定したcodeで識別し、platformが提供する場合だけ数値exit codeを保持する。
Error/Debug/outcomeにはendpoint、argv、input path、資格情報、raw stderr、JPEGを含めない。

このsessionは同期one-shotであり、再接続、指数backoff、health/state遷移、Tokio、Tauri、
HTTP、Banto-HUBを実装しない。restart supervisorとアプリ終了時の上位制御は次スライスに残す。

### crate-internal interruptible control

restart supervisorの前提として、crate内部だけにclone可能なstop signal/token pairと
`run_preserving_stores_until`を置く。公開`run`のAPIとone-shot close semanticsは変えない。
tokenを監視するsessionは約25ms周期でstopを確認し、要求を観測したらchildを
terminate/reapしてからstdout/stderr workerを両方joinする。stopは通常のinternal
`Stopped` completionであり、killが生成したnon-success exitを公開エラーへ変換しない。
frame storeとdiagnosticsはstop・失敗を問わずpreserveし、close責務は将来のsupervisorが持つ。

観測順序は、childのtry-wait/reap、既にqueueへ通知済みのworker結果、stopの順とする。
run開始前から要求済みのstopは、最初のtry-wait確認後にworker通知より先に扱う。したがって
try-wait/terminateのlifecycle errorはstopより優先され、stop観測前のpump失敗・panicも
元のstructured errorを維持する。stop後の正常EOFとkill由来exitだけを`Stopped`へまとめ、
pump errorやpanic自体はjoin時に引き続きエラーとして扱う。このsliceはstopを消費するだけで、
retry、backoff、状態遷移、複数session loopは実装しない。

## 第五Aスライス: restart-supervisor control core

`VideoSupervisor`は一つのowner threadと`VideoSupervisorHandle`を持ち、handleからのstopを
idempotentに受け付ける。stopはactive sessionのinternal stop signalとbackoff用Condvarの
双方へ通知するため、session実行中と再接続待機中のどちらもbusy-waitなしで中断できる。
ownerの`stop_and_join`とDropはthreadをjoinし、最終終了時にtop-level ownerとしてframe
storeとdiagnosticsを一度だけcloseする。各one-shot attemptはpreserve modeを使う前提であり、
再接続ごとにstable consumer handleを作り直さない。

owner thread境界はcontrol core、factory、waiterからのpanicをcatchし、panic payloadを公開せず
`SupervisorThreadPanicked`へ変換する。その場合もstatusを`Stopped`へ戻して両storeをcloseする。
thread spawn前はowner側にもstore producerを保持し、注入可能なspawnerが失敗した場合は、拒否された
closureのDropへ依存せずowner側で両storeを明示closeする。cleanup結果の優先順位はcore/spawnの
primary error、frame store close、diagnostics closeの順とし、後続cleanupは先行errorがあっても
必ず実行する。したがってspawn errorとclose errorが共存する場合はspawn errorを返す。

共有`VideoStatus`は初期・最終が`Stopped`、初回attemptが`Connecting`、以後のattemptと
delay中が`Reconnecting`、first-frame reporterの通知後が`Live`となる。first frameは
`last_frame_at`を更新してfailure count/errorをresetし、そのlive sessionが後で失敗した場合は
failure countを1から再開する。失敗はsaturating incrementし、公開状態には`RtspErrorInfo`だけを
保存して文字列を保持しない。現時点ではconfiguration validationをloop外で済ませた前提で、
factory生成失敗を含む既存session/launch errorをすべてretryableとして扱う。即時失敗や正常EOF
でも必ず`ReconnectPolicy`のdelayを通し、tight loopを作らない。

並行stop/failureでは、attemptが返したfailureをstatusへ記録した後、stopが次attemptのscheduleを
抑止する。active session自身が`Stopped`を返した場合も正常終了とし、追加attemptを作らない。
backoff indexは0から厳密に進め、first frame後は0へresetする。production FFmpeg executable、
runtime directory、input-file factory、実session attempt adapterは第五Bで接続する。このsliceの
factory/waiter seamはcrate-privateのdeterministic test用である。

## 第五Bスライス: production FFmpeg wiring

`FfmpegSupervisorOptions`は`RtspConfig`、FFmpeg executable、caller所有runtime directory、
JPEG byte上限、diagnostics件数/entry byte上限を保持する。constructorと開始直前の二段階で
empty executable、decoder/diagnostics bounds、runtime directoryの存在・directory種別・
短命probe fileのcreate/write/remove可否を検証し、失敗時はsupervisor threadを開始しない。
directory自体やACLはcrateが作成・変更しない。Windowsではapplication identityとadministrators
だけが読めるprotected ACLをcallerが事前に設定し、Unixの個別input fileはmode 0600で作成する。

各retry attemptはprocess IDとatomic nonceから非秘密filename候補を作り、`create_new`競合時だけ
最大32回まで次候補へ進む。認証URLはこの入力ファイル内だけに書き、shellを介さない
`FfmpegCommandSpec`、fresh decoder/sanitizer、stable frame/diagnostics producerを組み合わせて
interruptible one-shot sessionを開始する。command作成・spawn・stop・pump failure・panic・
normal exitの全経路でinput guardをDropし、最初のframeではpublishとinput cleanupが成功してから
同じ`received_at`をsupervisorへ一度だけ通知する。static validation errorはretry loop外、
attempt中のlaunch/session errorは第五Aの現行方針どおりretry対象とする。
stderr sanitizerには認証情報と完全endpointに加えてendpoint host/resource、executable、
runtime directory、attempt input pathの表記揺れpatternも渡し、公開diagnosticsへ接続先や
ローカルpathを残さない。

`RtspVideoSource`は`VideoSupervisor`を所有し、`LatestFrameHandle`、
`FfmpegDiagnosticsHandle`、`VideoSupervisorHandle`だけをconsumerへ公開する。producer storeは
公開せず、handleは再接続をまたいで同じbounded stateを参照する。`request_stop`は冪等、
`stop_and_join`とDropはsupervisorを停止・joinし、最終ownerだけが両storeをcloseする。
FFmpeg sidecarのversion/license固定、実機RTSP接続、長時間稼働、
Tauri binary IPC/配布配線は後続のintegration作業に残す。

## Phase 1の状態と検証

Phase 1ではendpoint/credentials分離、必須finite I/O timeout付き短命ffconcat manifest、
shellなしFFmpeg起動、MJPEG
frame decode/latest-frame、sanitized bounded diagnostics、one-shot session、interruptible stop、
restart supervisor、production source compositionまで実装した。認証URLはargvへ含めない。
ローカルFFmpeg 9.0.1で旧 `-/i` 方式が非対応と判明したため、上流Issueにはせず、未コミット
差分内でffconcat方式へ修正した。今回起票した上流Issueはない。

上位レビューと独立検証では、`cargo fmt --all --check`、`cargo test -p banto-rtsp`
（102 passed、2 ignored）、Gyan FFmpeg 9.0.1を指定したignored capability tests
（offline JPEGとaccept後無応答RTSP timeoutの2 passed）、
`cargo clippy -p banto-rtsp --all-targets -- -D warnings`、`cargo check --workspace`、
`git diff --check`、実機秘密文字列scanが成功した。したがって現在の粒度は
**コード/ローカルcapability完了、実機/配布確認待ち**であり、全プロジェクト完了ではない。

実機RTSP接続、Windows protected ACLの実運用確認、最終FFmpeg配布元・ライセンス判断、
Tauri binary IPC、縦回転UIは未完了である。次工程はprivate Tauri骨格と薄いadapterだが、
着手前にアプリ名、Tauri identifier、privateリポジトリ内の配置を確定する。

## 公開する基礎契約

- `RtspEndpoint`: `rtsp://` / `rtsps://` のみを許可し、authority の userinfo、
  空 host、ASCII control 文字を拒否する。
- `RtspCredentials`: username/password を endpoint から分離する。Debug は双方を
  redact する。
- `RtspConfig`: endpoint、資格情報、transport、必須finite I/O timeout、再接続方針を
  まとめるが、接続は開始しない。
- `ReconnectPolicy`: 整数演算の指数バックオフ。初期値・最大値・係数を検証し、
  大きな attempt でも最大値へ飽和する。
- `JpegFrameDecoder`: chunk 境界、noise、複数フレーム、nested SOI、上限超過後の
  復旧を扱う。
- `VideoFrame` / `VideoStatus`: sequence、受信時刻、JPEG payload、映像状態と
  非秘密の構造化エラー情報を表す。
- `FfmpegChild`: shell を介さず FFmpeg child と piped stdio を所有し、終了・reap
  を明示的に扱う。
- `LatestFrameStore` / `LatestFrameHandle`: 最新一枚だけの保持と、read-only consumer
  handle による snapshot/wait を提供する。
- `FfmpegSupervisorOptions` / `RtspVideoSource`: static validation済みのproduction FFmpeg
  ownerと、再接続をまたいで安定したread-only handleを提供する。

## 境界と秘密情報

この crate は Tauri、axum/HTTP、Banto-HUB、Modbus の型を import しない。Tauri adapterは
上位層に置く。RTSP の認証情報は URL に
埋め込まず、製品アプリ側で OS keyring 等から取得して `RtspCredentials` に渡す。
この public repository へ実機設定や資格情報を追加しない。

## 次の設計判断

FFmpeg sidecarの最終配布元・バージョン・ライセンス、実機接続/切断/長時間稼働、Windows
protected ACL、Tauri binary IPC adapterと縦回転UIは、privateアプリの配布・運用要件を
確認してから決定する。private Tauri骨格へ進む前に、アプリ名、identifier、配置を確定する。
