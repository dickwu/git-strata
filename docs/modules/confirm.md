# `confirm` 模块

**Crate**：`crates/strata-confirm` · **设计来源**：design.md §2 目标 7、§3（一推就上线、库一推就部署）、§6.4 第 5 步、§6.5 第 5 步、§6.6、§6.9、§9 不变量 3 与 7、§11 第 2 条、§13 问题 2；types.md §4（`ConfirmToken`、`Action`，以及其后的"签发与校验""动作从哪来""谁消费"三条）、§6.2（`NotConfirmed`、`NotTty`、`Unsupported`）、§8；module-rulings.md 的「Common」「T12 confirm」与标了 T12、Everyone 的增补 · **矩阵**：没有目标模块为 `confirm` 的行；理由点名 confirm 的只有 3d `:337`（目标 daemon）。3e `sdk/js/test/packaging.mjs::dispatch`（`—`，不保留）的理由点名 design §6.9，不是本模块；3d 里"无需认证通道"的各行，逐条见 §7

> 本文按 module-rulings.md 写：「T12 confirm」；Addendum 1 的 T12 两条（"LocalAuthentication only…"，以及 T12/T13 的钉住规则与 `Action::ConfigRelax`）、T13 的钉住持久化两条（Addendum 1、Addendum 5）；Addendum 3、4 的错误时机与测试接缝；「A confirmation names the exact plan」「approve refuses a Pending whose head is no longer main's tip」「Lock order: session queue before repo lock」。`ConfirmToken` 与 `Action` 是 types.md §4 的纯数据，本文只引用、不重新定义；`Action::ConfigRelax { repo: RepoId }` 已在 types.md §4。签发顺序见 §3，是活的；§5.6 的那一行要写。TTY 检查 `require_tty` 是 cli 的，confirm 不看 TTY。confirm 不写任何文件、不改任何 ref、不碰任何工作区，只有进程内的状态，所以它的每一个失败都是具名拒绝，从不返回 `Unknown`（共同规则的错误时机，§6）。文中标"实测"的，是 2026-10-23（UTC）在本机（macOS 27.0.1，SDK 27.0，带 Touch ID）用会话临时目录里的 Swift 探针验证过的；探针从不弹框，每次评估都先设 `interactionNotAllowed = true`。标 [我的决定] 的地方是 design、types.md 和裁定都没有定、由本文决定的，§9 有汇总。

## 1. 职责与边界

confirm 是 design §6.9 那条"agent 无法代答的通道"在 daemon 里的一半。威胁模型（design §6.9、§13 问题 2）：agent 与人类同一个 uid，常以 bypass 权限运行；它能跑任何命令、设任何环境变量、加任何 flag、改配置文件、直接连 daemon 的 unix socket，还能用 `script(1)` 给自己开一个伪终端骗过 TTY 检查。它做不到的只有一件事：在系统认证框里给出人类的指纹或登录密码。confirm 把"人刚刚亲自确认了这个动作"变成一个只活在 daemon 内存里、一次一用、30 秒过期的凭据。

confirm 负责：

1. **弹框**：对一个 `Action` 弹一次 LocalAuthentication 系统认证框（`LAContext`，策略 `LAPolicyDeviceOwnerAuthentication`：Touch ID，没有或用不了时要登录密码；design §6.9）。每次一个新的 `LAContext`，不沿用先前的认证（`touchIDAuthenticationAllowableReuseDuration = 0`）；框上的文字逐字说出要做的动作（§5.6）。
2. **签发**（`Confirmer::require`）：认证通过之后签发 `ConfirmToken { action, issued, nonce }`，在进程内的签发登记表里记下它。认证失败、被取消、超时、被系统打断 → `NotConfirmed`；环境弹不出能确认人的框（不在图形会话里、账户没设登录密码）→ `Unsupported`。
3. **校验与核销**（`Confirmer::validate`）：nonce 是本签发者签发、还没核销过的；凭据签发之后没被改过；`action` 与 daemon 期望的完全相同；从认证通过算起不到 30 秒，系统睡眠的时间也算在内（§5.7）。不论成败，一个 nonce 只能被核销一次。
4. **探测**（`Confirmer::probe`）：不弹框地回答"现在能不能确认"，给 daemon 的 status 用。

confirm 不负责的事，以及它们的去处：

| 事项 | 归谁 |
| --- | --- |
| TTY 检查：确认类命令在 stdin 不是 TTY 时，在发出请求之前就返回 `NotTty`（design §6.9）。计划写的 `fn require_tty() -> Result<(), NotTty>` 的签名由 cli.md 定，码是 types.md 的 `Code::NotTty` | cli（module-rulings「T12 confirm」「T14 cli」与 Addendum 1 的 T12 一条） |
| 判断一个请求要不要确认：看 daemon 钉住的 `deploy_on_push`、`publish.confirm`、`publish.push`（config.md §1 的钉住规则与 `RepoConfig::needs_human_confirm`），不看请求本身，也不看刚被改过的配置文件；从请求推出 `Action`，从不采用客户端传来的；何时调 `require`、`validate`；拿到锁之后再核对一次；把凭据按值交给消费方 | daemon（daemon.md；types.md §4"动作从哪来"；本文 §5.1 写出它要守的顺序） |
| 配置调松时的确认（`Action::ConfigRelax`）：发现调松、调松之前继续用钉住的值、由哪条命令触发确认 | daemon 与 cli（config.md §1；module-rulings Addendum 1 的 T12/T13 一条、Addendum 5 的 T13 一条）。签发与核销见 §3；confirm 不写钉住值 |
| 按值收下凭据并核对 `action`，不符返回 `NotConfirmed` | session（`remove` 收 `RemoveDiscard`）、publish（`push` 收 `PushDeploy`）（types.md §4"谁消费"） |
| 提议还是不是 Pending；批准前提议被取代或主干已移动时，把替换它的那一条交给人重新确认 | deps（deps.md §5.12）与 daemon、cli（module-rulings「A confirmation names the exact plan」） |
| 审计：推送时核对过的 `Action` 记进 `refs_audit.confirmed`，从不记凭据 | publish 写、store 存（store.md 的 `RefChange.confirmed`） |
| `ConfirmToken`、`Action` 的定义 | types.md §4（`strata-types`） |

对应 design 的章节：§6.9 全节；§9 不变量 7，以及不变量 3 里"一推上线需人类确认"的那一半；§13 问题 2 的选择 (a)；§11 第 2 条（macOS API 走 objc2）与 §6.9 的"[实现：objc2 的 LocalAuthentication 绑定，交给构建者]"（选择记在 §9）。三个调用点是 §6.4 第 5 步的 `deps approve`、§6.5 第 5 步的一推上线推送、§6.6 的 `rm --discard`。design §11 第 6 条的 `--confirm-deploy` 若由 cli 保留，它只是一个表明意图的 flag，换不来凭据（§7 的 `no_env_or_flag_bypass` 覆盖它）。

本文不防、也防不住的：agent 改写并重装 strata 自己的二进制或 daemon；agent 绕开 strata，直接用 git 在人的主 checkout 里推送（那是人自己的仓库与凭据，不变量 2 约束的是 strata）；人把登录密码放在 agent 读得到的地方。不变量 7 管的是 strata 自己的路径：经 strata 的 CLI、RPC、配置与环境变量，都绕不过确认。

## 2. 依赖

Workspace crate（types.md §8：`strata-confirm` 只依赖第 0 层；只有 `strata-daemon` 依赖它，`git-strata` 不依赖它）：

| 依赖 | 用途 |
| --- | --- |
| `strata-types` | `Action`、`ConfirmToken`、`StrataError`、`Code`；拼框上文字时用 `RepoName`、`SessionId`、`ProposalId`、`RepoId` 的 `Display`（`ConfigRelax` 的 `repo` 是 `RepoId`） |

外部 crate（都只在 macOS 上编译；design §11 第 2 条）：

| 依赖 | 理由 |
| --- | --- |
| `objc2-local-authentication` 0.3.2（`default-features = false`，features `std`、`LAContext`、`LAError`、`block2`） | `LAContext` 的 `new`、`canEvaluatePolicy_error`、`evaluatePolicy_localizedReason_reply`、`invalidate`、`setTouchIDAuthenticationAllowableReuseDuration`、`localizedFallbackTitle`、`setInteractionNotAllowed`（只在测试接缝里设为 true）；`LAPolicy::DeviceOwnerAuthentication`；LAError 的各个常量与 `LAErrorDomain`（§5.3、§5.4）。这是 objc2 项目从 SDK 头文件逐个生成的绑定；0.3.2 发布于 2025-10-04，是 crates.io 上的最新版（2026-10-23 查） |
| `objc2` 0.6（feature `exception`） | `runtime::Bool`、`rc::Retained`、`rc::autoreleasepool`；`exception::catch` 把 Objective-C 异常变成 `NotConfirmed`，不让它展开进 Rust（§5.2 第 6 步） |
| `objc2-foundation` 0.3.2（features `NSString`、`NSError`） | 框上文字的 `NSString::from_str`；回复里错误对象的 `domain()`、`code()` |
| `block2` 0.6 | 回复块 `RcBlock::new`。`evaluatePolicy` 异步回调，绑定的安全要求是回复块可以跨线程发送 |
| `objc2-security` 0.3.2（`default-features = false`，feature `AuthSession`） | `SessionGetInfo(callerSecuritySession, …)` 与 `SessionAttributeBits::sessionHasGraphicAccess`：弹框之前判断 daemon 是否在有图形能力的安全会话里（§5.2 第 3 步） |
| `libc` | `clock_gettime(CLOCK_MONOTONIC)`（睡眠时也走的单调时钟，§5.7）；`arc4random_buf`（nonce，§5.8） |
| `serde_json` | `StrataError.detail` |
| 开发依赖 `static_assertions`、`regex` | §7 的类型性质断言；源码扫描里找 `STRATA_` 名字与 `ConfirmToken {` 字面量 |

依赖方向符合 types.md §8：不依赖 apfs、gitio、store 或任何更高层的 crate，也不跑子进程、不读文件、不连 socket。

谁调用本模块：

| 调用方 | 用到 |
| --- | --- |
| daemon | 进程里唯一一个 `Confirmer`（`Confirmer::new`）；处理需要确认的请求时先 `require` 再 `validate`（§5.1）；status 里的确认通道状态用 `probe`；启动时看 `TEST_SEAMS`（§4.2 C8）；测试里用 `Confirmer::with_seams` |
| session、publish | 不调用本模块（types.md §8）。它们按值收下 `ConfirmToken`、核对 `token.action`：session 见 session.md §5.6 第 2 步与测试 `remove_discard_checks_the_token`（sdd/task-9 5797ee8）；publish 的 `push` 由 Task 10 按 types.md §4 写 |
| deps | 不调用，也不收凭据（deps.md 第 23 行，sdd/task-8 74dd1c9）；`deps approve` 的确认由 daemon 在 deps.md §5.12 第 2 步做（同一文档第 902 行） |
| store | 不调用。`refs_audit.confirmed` 只存核对过的 `Action`，从不存凭据（store.md 第 23、437、670、1524 行，本分支 08ffa53） |
| cli | 不依赖本模块；`require_tty` 是它自己的。§7 的两个端到端测试放在它的测试目录里，经 daemon 的测试接缝看 confirm 的弹框记录（§8 Task 6） |

## 3. 公开接口（Rust）

`ConfirmToken`、`Action` 原样引用 types.md §4：`ConfirmToken` 只 derive `Debug`，不能复制、不能序列化。本节定义 confirm 自有的公开项：`Confirmer`、四个时长常量、`TEST_SEAMS`、`prompt_reason`，以及只在 `test-seams` feature 下出现的 `seams` 模块。

```rust
// crates/strata-confirm/src/lib.rs —— 签名清单（不是实现）
use std::time::Duration;
use strata_types::{Action, ConfirmToken, StrataError};

/// types.md §8 的 `Result<T>`。
pub type Result<T> = std::result::Result<T, StrataError>;

/// 凭据的有效期：从认证通过、凭据签发那一刻起算，到 validate 为止（计划 Task 12：30 s）。
/// 按两个时钟各算一次年龄，取较大者：Instant（睡眠时不走）与 CLOCK_MONOTONIC（睡眠时也走），§5.7。
pub const TOKEN_TTL: Duration = Duration::from_secs(30);

/// 一次弹框最多等多久，按睡眠时也走的时钟算；到点 invalidate 这个 LAContext，返回 NotConfirmed（"timeout"）。
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// invalidate 之后，等框架送回迟到的回复最多多久；过了就放下这个上下文。
pub const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// 签发登记表留一条记录多久（从签发起算）。远大于 TOKEN_TTL，
/// 这样过期与用过的凭据报得出 "expired"、"used"，而不只是 "unknown"（§4.1）。
pub const RECORD_RETENTION: Duration = Duration::from_secs(600);

/// 本 crate 是否带着测试接缝编译。git-stratad 的 main 见到 true 就拒绝启动（daemon.md），
/// 所以装到机器上的 daemon 里不会有脚本化的认证器（§4.2 C8）。
pub const TEST_SEAMS: bool = cfg!(feature = "test-seams");

/// daemon 持有的唯一签发者。Send + Sync；内部两把互斥锁（弹框闸门、签发登记表），从不等着拿第二把（§5.9）。
pub struct Confirmer { /* 私有：认证器、时钟、登记表、闸门、弹框期限 */ }

impl Confirmer {
    /// 生产构造：认证器是真的 LocalAuthentication（§5.3），时钟是真时钟。
    /// 不做 I/O、不弹框，不读任何环境变量或配置。
    pub fn new() -> Confirmer;

    /// 对 action 弹一次系统认证框，通过就签发一个凭据并登记它的 nonce。
    /// 阻塞调用线程，最长 PROMPT_TIMEOUT + CANCEL_GRACE（外加调度误差）；
    /// daemon 在阻塞线程池里调，调用时不持有任何 repo 写锁或会话队列位置（§5.1）。
    /// 码：NotConfirmed（detail.reason：cancelled、failed、interrupted、timeout、prompt_in_progress、
    ///     unexpected、la_exception、internal）；
    ///     Unsupported（no_gui_session、session_info、passcode_not_set、not_interactive、cannot_evaluate）。
    pub fn require(&self, action: Action) -> Result<ConfirmToken>;

    /// 核销一个凭据：nonce 由本签发者签发、还没核销过；token 的 action、issued 与登记的相同；
    /// 登记的 action 等于 expected；年龄小于 TOKEN_TTL。登记表里有这个 nonce 时，不论结果如何都把它记为已用。
    /// 成功时凭据仍归调用方，由它按值交给消费方。
    /// 码：NotConfirmed（detail.reason：unknown、used、tampered、mismatch、expired、internal）。
    pub fn validate(&self, token: &ConfirmToken, expected: &Action) -> Result<()>;

    /// 不弹框地探测确认通道：图形会话 + canEvaluatePolicy（§5.2 第 3、4 步）。给 daemon 的 status 用。
    /// 码：Unsupported（同 require 的 Unsupported 各项）。
    pub fn probe(&self) -> Result<()>;
}

/// 框上的文字（§5.6）：系统把它拼进 "<进程名>" is trying to <这段文字>.
/// 纯函数；结果非空，只含可打印 ASCII，其余字符转义。
pub fn prompt_reason(action: &Action) -> String;

/// 测试接缝（cargo feature `test-seams`，只在测试里打开，从不进发布的二进制，Addendum 4）。
#[cfg(feature = "test-seams")]
pub mod seams {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    /// 脚本里一次弹框的结局。
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Verdict {
        /// 回复 (success, error)；error 为 Some((domain, code))，domain 写 "com.apple.LocalAuthentication" 即 LAErrorDomain。
        Reply { success: bool, error: Option<(String, isize)> },
        /// 一直不回复；被 invalidate 之后回 AppCancel（-9），与真框架相同（LAContext.h）。
        Hang,
        /// 一直不回复，被 invalidate 之后也不回复。
        Silent,
        /// 一直不回复；被 invalidate 之后却回 success = YES（"到点那一刻人刚好通过"）。
        PassAfterCancel,
        /// evaluatePolicy 抛 Objective-C 异常。
        Throw,
    }
    impl Verdict {
        /// 人通过了认证：Reply { success: true, error: None }。
        pub fn pass() -> Verdict;
        /// success = NO，error = (LAErrorDomain, code)。
        pub fn la(code: isize) -> Verdict;
    }

    /// 脚本化的认证器：图形会话、预检结果与每次弹框的结局都由测试给定，并记下它看到的一切。
    pub struct ScriptedAuth { /* 私有 */ }
    impl ScriptedAuth {
        /// gui：SessionGetInfo 是否报图形能力；can_evaluate：canEvaluatePolicy 的结果（Err 带 LAError 的码）；
        /// verdicts 依次用于每次弹框，用完之后一律是 Verdict::la(-2)（人点了取消）。
        pub fn new(gui: bool, can_evaluate: std::result::Result<(), isize>, verdicts: Vec<Verdict>) -> ScriptedAuth;
        /// 与这个认证器共享的记录。
        pub fn log(&self) -> Arc<PromptLog>;
    }

    /// 认证器看到的一切；每个读取方法返回快照。
    #[derive(Debug, Default)]
    pub struct PromptLog { /* 私有：一把 Mutex 包着下面四项 */ }
    impl PromptLog {
        /// 每次弹框的文字，按顺序。
        pub fn shown(&self) -> Vec<String>;
        /// invalidate 的总次数（每个上下文用完都 invalidate，§5.2 第 10 步）。
        pub fn invalidated(&self) -> u32;
        /// 评估还没回复时就 invalidate 的次数（只有到期这一条路，§5.2 第 7 步）。
        pub fn cancelled_while_pending(&self) -> u32;
        /// 图形会话探测与预检被调用的次数。
        pub fn probes(&self) -> u32;
    }

    /// 可拨动的真时钟：两个读数都随真实时间前进，再加上测试拨进去的偏移。
    /// advance：两个时钟一起往前拨；sleep：只拨 CLOCK_MONOTONIC 那一个（模拟系统睡眠）。
    #[derive(Clone, Debug)]
    pub struct FakeClock { /* 私有：Arc<Mutex<两个偏移>> */ }
    impl FakeClock {
        pub fn new() -> FakeClock;
        pub fn advance(&self, d: Duration);
        pub fn sleep(&self, d: Duration);
    }

    /// 真的 LocalAuthentication 的变体。quiet() 永不弹框：评估前设 interactionNotAllowed = true
    /// （LAContext.h：需要界面时以 LAErrorNotInteractive 失败）；invalidate_first 时评估前先 invalidate。
    #[derive(Clone, Copy, Debug)]
    pub struct LaOptions {
        pub interaction_not_allowed: bool,
        pub invalidate_first: bool,
        pub prompt_timeout: Duration,
    }
    impl LaOptions {
        /// interaction_not_allowed = true，invalidate_first = false，prompt_timeout = PROMPT_TIMEOUT。
        pub fn quiet() -> LaOptions;
    }

    /// 新建一个 LAContext、按生产配置设好、读回、再 invalidate（不评估，不弹框）。
    #[derive(Clone, Debug, PartialEq)]
    pub struct LaConfigReadback {
        pub reuse_duration: f64,
        pub interaction_not_allowed: bool,
        /// localizedFallbackTitle 为 nil：保留系统的"使用密码…"回退。
        pub fallback_title_is_default: bool,
    }
    pub fn read_back_la_config() -> LaConfigReadback;

    /// §7 两个"绕不过"测试共用的环境变量名单：workspace_root/crates 下源码里出现的每个
    /// STRATA_[A-Z0-9_]+ 名字，并上一组固定的诱惑名（§7 confirm_reads_no_environment）。
    pub fn bypass_env_names(workspace_root: &Path) -> Vec<String>;

    impl super::Confirmer {
        /// 脚本化认证器 + 可拨动时钟；prompt_timeout 与 cancel_grace 可以调短。
        pub fn with_seams(auth: ScriptedAuth, clock: FakeClock,
                          prompt_timeout: Duration, cancel_grace: Duration) -> super::Confirmer;
        /// 真 LocalAuthentication + 真时钟，按 opts 配置。
        pub fn with_la(opts: LaOptions) -> super::Confirmer;
        /// 登记表里当前的记录数。
        pub fn record_count(&self) -> usize;
    }
}
```

**与计划签名的差别**：

- 计划写的是自由函数 `fn require(action) -> Result<ConfirmToken, StrataError>`，这里是 `Confirmer::require(&self, action)` [我的决定]。一次一用要靠签发登记表，登记表是状态：放进 daemon 持有的一个对象里，比进程全局的 static 好测；两个 `Confirmer`（例如 daemon 重启前后）互不承认对方的凭据。`validate` 就是 types.md §4 要的"经 strata-confirm 校验它没用过、没过期，并核销它的 nonce"。
- 计划的 Produces 列了 `struct ConfirmToken`、`enum Action`：它们已经在 types.md §4，本文只引用（module-rulings「T12 confirm」）。
- 计划的 `fn require_tty() -> Result<(), NotTty>` 不在本 crate：TTY 检查归 cli（module-rulings「T12 confirm」「T14 cli」）。types.md 里没有叫 `NotTty` 的类型，只有 `Code::NotTty`；签名由 cli.md 定。
- 多出来的 `validate`、`probe`、`prompt_reason`、常量、`TEST_SEAMS` 与测试接缝，各有调用方（§2）。

各函数的语义与可能返回的 `Code`：

- **`require`**：§5.2 到 §5.4。只返回 `NotConfirmed` 与 `Unsupported`，`detail` 形如 `{"reason": "cancelled", "action": <Action>, "la_code": -2}`；`la_code` 只在框架给了 LAError 的码时出现。从不返回 `Unknown`：confirm 不写任何对外可见的东西（§6）。
- **`validate`**：§5.5。只返回 `NotConfirmed`；`mismatch` 的 `detail` 是 `{"reason", "expected": <Action>, "confirmed": <Action>}`，`expired` 的带 `age_ms`。
- **`probe`**：§5.2 的第 3、4 步单独跑一遍，不建回复块、不评估。
- **`prompt_reason`**：纯函数，不失败（§5.6）。
- **`Confirmer::new`**：不失败。真认证器在每次 `require` 时才碰 LocalAuthentication，所以 daemon 在没有图形会话的地方也能启动，只是确认类请求得到 `Unsupported`。

`Action::ConfigRelax { repo: RepoId }` 在 types.md §4（module-rulings Addendum 1）。本文不重新定义 `Action`。下面的签发是活的。`prompt_reason` 对 `Action` 的 match 不写通配分支 [我的决定]：缺了 §5.6 表里那一行，confirm 就编译不过，框上的文字不会退化成一句笼统的"确认一个操作"。那一行要写。`validate` 按 `Action` 的 `PartialEq` 比较，不为这个变体另写比较。

签发（发现调松、调松之前继续用钉住的值、由哪条命令触发，仍归 daemon 与 cli；哪边算调松见 config.md §1）：

1. 调紧钉住的 `deploy_on_push`、`publish.confirm` 或 `publish.push` 立即生效。不弹框，不签发凭据。
2. 调松其中任何一个都不生效，直到人跑一条显式命令。daemon 不得因为配置文件变了就自己弹框（§9）。
3. 那条命令调用 `require(Action::ConfigRelax { repo })`，走与其他动作相同的 LocalAuthentication 路径（§5.2 到 §5.4）。框上的文字是 §5.6 表的最后一行。
4. 随后 `validate` 核销的就是这同一个 `Action`。`expected` 不是它 → `NotConfirmed`（`mismatch`），nonce 已用掉，与其他动作相同（§5.5 第 3、5 步；§7 的 `wrong_action_is_refused`）。
5. daemon 是这张凭据的消费方，不是 confirm：`validate` 核销之后，它才写入调松后的钉住值。confirm 不写钉住值，也不写任何文件。核销之后写失败，凭据已经用掉，人要重新确认（§5.1 第 5 步）。

## 4. 数据与不变量

### 4.1 进程内状态

confirm 没有持久数据：不建表、不写文件，在 Store 里没有行（module-rulings Addendum 4：schema 只属于 store，confirm 也用不着）。"这次推送经过了什么确认"由 publish 以 `Action` 的形式写进 `refs_audit.confirmed`，凭据本身从不落盘（types.md §4；store.md 的 `RefChange.confirmed`）。

`Confirmer` 里有：

| 部分 | 内容 | 说明 |
| --- | --- | --- |
| 签发登记表 | `Mutex<HashMap<u128, Record>>`，`Record { action: Action, issued: Instant, issued_mono_ns: u64, spent: bool }` | 签发时插入（`spent = false`）；`validate` 把它改成 `spent = true`，不删；每次 `require`、`validate` 先清掉签发时刻（按 CLOCK_MONOTONIC）早于 `RECORD_RETENTION` 的记录 |
| 弹框闸门 | `Mutex<()>`，`require` 用 `try_lock` | 同一时刻至多一个框（C6） |
| 认证器 | crate 私有的 `Authenticator`：`gui_session`、`preflight`、`evaluate`、`invalidate` 四个操作；生产实现 `LocalAuth`（§5.3），测试是 `ScriptedAuth` 或 `with_la` 的变体 | 生产构造只接真认证器 |
| 时钟 | 生产读 `Instant::now()` 与 `clock_gettime(CLOCK_MONOTONIC)`；测试是 `FakeClock` | §5.7 |
| 弹框期限 | 生产是 `PROMPT_TIMEOUT`、`CANCEL_GRACE`；测试可以调短 | §5.2 第 7 步 |

daemon 重启或换一个 `Confirmer` 之后，旧凭据一律是 `unknown`：它们只活在旧进程的内存里（types.md §4）。

### 4.2 不变量

| 编号 | 不变量 | 落实 design §9 |
| --- | --- | --- |
| C1 | confirm 签发的每个 `ConfirmToken`，都对应一次在期限之内收到的 LocalAuthentication 成功回复：策略 `DeviceOwnerAuthentication`、全新的 `LAContext`、`touchIDAuthenticationAllowableReuseDuration = 0`、框上的文字是 `prompt_reason(&token.action)`。签发没有别的路径：生产构造只接真认证器，签发步骤不公开 | 7；以及 3 的一推上线确认 |
| C2 | 每个 nonce 至多一次 `validate` 成功；任何一次 `validate` 只要在登记表里找到它，就把它记为已用 | 7；design §6.9 "一次一用，不缓存" |
| C3 | `validate` 成功，当且仅当：凭据的 `action`、`issued` 与登记的相同，登记的 `action == *expected`，并且按两个时钟算的年龄都小于 `TOKEN_TTL` | 7 |
| C4 | confirm 的输入只有 `Action` 和 LocalAuthentication 的回复：不读环境变量（运行期的 `std::env`、编译期的 `env!`、`option_env!` 都没有），不读配置或文件，不看 TTY，不看请求方是谁，也没有任何参数能跳过弹框。测试接缝只跟 `test-seams` feature 走，不跟 `debug_assertions` 或 `cfg(test)` 走 | 7：没有任何 flag、环境变量或配置能绕过 |
| C5 | 凭据不出 daemon 的内存：confirm 从不序列化、存盘或发送凭据；`ConfirmToken` 本身也不能 Clone、不能序列化（types.md §2.2、§4；§7 用静态断言钉住） | 7 |
| C6 | 同一时刻至多一个框；框开着时来的第二个 `require` 立即以 `NotConfirmed`（`prompt_in_progress`）拒绝，不排队 [我的决定]。一个刷请求的客户端排不出一串框，人也不会在一串框里顺手点过一个不是自己要的 | 7 |
| C7 | confirm 的任何失败都不留下现场：它不写文件、不改 ref、不碰工作区，没有凭据就什么都没发生 | 6 |
| C8 | 装到机器上的 `git-stratad` 里没有测试接缝：`TEST_SEAMS` 为 true 的二进制拒绝启动（daemon.md 实现），§7 的 `release_daemon_has_no_test_seams` 检查发布构建的 feature | 7 |

调用方要守的规矩。confirm 防不住它们，写在这里给 daemon.md 照做：

- **D1**（types.md §4）：凭据交给消费方之前，先 `validate`；交出去的每一个凭据都经过它。session 与 publish 只核对 `action`，它们不能、也不该调用 confirm。
- **D2**（types.md §4、module-rulings「T12 confirm」）：拿到锁之后、真正动手之前，daemon 按锁里重新解析出的请求再推出一次 `Action`，与 `token.action` 比较；不同就 `NotConfirmed`，什么都不做。选择器（ticket → 会话 id）在锁外与锁里可能解析出不同的东西，这一步就是为它。
- **D3**（config.md §1）：[实现时补的决定]（2026-10-26，peilin："将api 也改为不用确认"）：要不要确认只看钉住的 `publish.confirm`（`deploy_on_push` 只是信息，config.md V5 撤销）；下文的钉住与调松规则不变。原文：一个推送要不要确认，看 daemon 钉住的三个值，不看请求，也不看刚被改过的配置文件。调紧钉住的 `deploy_on_push`、`publish.confirm` 或 `publish.push` 立即生效，不弹框、不签发；调松任何一个都不生效，直到人跑显式命令走 §3 的 `ConfigRelax` 签发。daemon 不因文件变化自己弹框。

## 5. 流程与算法

### 5.1 调用约定：daemon 的顺序

[我的决定。daemon.md 照做，或在它的 §2 写明要改的地方。]

1. 收到请求。cli 已经做过 `require_tty`，但请求也可能来自直接连 socket 的客户端：daemon 不信任客户端声称的任何东西，RPC 参数里也没有能代表"已确认"的字段。
2. 解析选择器，先做不需要确认就能下结论的具名拒绝：会话不存在、ticket 有歧义、提议不是 Pending（deps.md §5.12 第 1 步：不弹框）、这个推送按钉住的值根本不需要确认……注定失败的请求不弹框。
3. 推出 `Action`（D3）。一个请求要多个确认时（一个发布组里有两个要确认的推送目标），每个 `Action` 各弹一次（types.md §4、design §6.9），依次进行。
4. `confirmer.require(action.clone())`：在阻塞线程池里调，调用时**不持有任何 repo 写锁或会话队列位置**。一次弹框可以等到 `PROMPT_TIMEOUT`（60 s），比写锁的有界等待 30 s（module-rulings「T13 daemon」）还长；持着写锁弹框，同一 repo 的其他写者会在人犹豫的时候 `Busy`；持着会话队列位置弹框，这个会话的 ToolCall checkpoint 就排到人后面去了（module-rulings「Lock order: session queue before repo lock」）。cli 等 RPC 回复的上限要大于 `PROMPT_TIMEOUT + CANCEL_GRACE`（cli.md）。
5. 立刻 `confirmer.validate(&token, &action)`，核销 nonce。从这一步起这个确认已经用掉：后面任何一步失败（`Busy`、`OpenFiles`、提议被取代……），人要重新发命令、重新认证（design §6.9 "不缓存"）。
6. 按「Lock order」占会话队列位置、拿 repo 写锁；做 D2 的再核对；把凭据按值交给消费方：`session::remove(id, true, Some(token))`、`publish::push(…, token)`。`deps approve` 没有消费方，daemon 在 D2 之后接着做 deps.md §5.12 的第 3 到 8 步（types.md §4："`DepsApprove` 由 daemon 处理 `deps approve` 时校验一次"）。
7. 推送的结果连同 `confirmed = Some(action)` 由 publish 记进 `refs_audit`（store.md 的 `record_ref_change`），凭据本身不记。

`TOKEN_TTL` 管的是第 4 步认证通过到第 5 步核销之间。正常情况下这段只有毫秒，30 秒的余量挡的是"签了先放着、过一会儿再用"这类代码路径。操作本身（冷装、跑检查、等锁）发生在核销之后，不受 30 秒限制，也不需要受：凭据已经被按值移走，不会再被用第二次。

确认的东西必须就是要做的东西（module-rulings「A confirmation names the exact plan」）：

- `DepsApprove { proposal }` 指名一条提议，而提议的目标与依赖集登记之后不再改变。提议在认证或冷装期间被取代时，deps 返回 `NoSuchProposal`，`detail.state` 指向替换它的那一条（deps.md §5.12 第 3、4 步）；daemon 与 cli 把替换提议的计划给人看，批准它是一次新的 `deps approve`、一次新的弹框。批准时主干已经移动（`HeadMoved`，reason `main_moved`，module-rulings「approve refuses a Pending whose head is no longer main's tip」；types.md §6.2 已写上这一含义）也一样。confirm 不另造一个码，跟着 deps 用这一个。confirm 从不把提议 P 的凭据改投 Q：`validate` 对它报 `mismatch`。
- `PushDeploy { repo, branch }` 不带 commit。推上去的必须就是人确认时看到的那一段：例如在第 2 步把发布链顶读出来给人看，推送前再核对它没动，动了就拒绝、重新确认。这是 publish.md 与 daemon.md 的事，本文只指出这个缺口（§9 末尾列为给 T10、T13 的问题）。
- `ConfigRelax { repo }` 走上面的第 4、5 步，顺序见 §3。`validate` 核对的就是这次 `require` 的那个动作；不符是 `NotConfirmed`（`mismatch`），nonce 已用掉。daemon 是消费方，核销之后才写钉住值；confirm 不写。调紧不进这两步。

### 5.2 `require(action)`

1. `reason = prompt_reason(&action)`（§5.6）。它非空：`localizedReason` 为空时 LocalAuthentication 抛 `NSInvalidArgumentException`（LAContext.h）。
2. 闸门：`gate.try_lock()`。已被占（另一次弹框正在进行）→ `NotConfirmed`（`prompt_in_progress`），不排队、不弹框（C6）。闸门的锁中毒（上一次弹框的线程 panic 了）按没中毒处理：它不保护数据，只做互斥。从这里到第 10 步一直持有闸门。
3. 图形会话：`SessionGetInfo(callerSecuritySession, &mut id, &mut attrs)`。返回值不是 0 → `Unsupported`（`session_info`，`detail.status`）；`attrs` 没有 `sessionHasGraphicAccess`（0x0010）→ `Unsupported`（`no_gui_session`，`detail.attrs`）。Security 框架把每个进程放进一个安全会话：图形登录与 ssh 登录各建一个，系统 daemon 在既没有用户也没有图形能力的 root 会话里（AuthSession.h）。launchd agent 只有加载在用户的 Aqua 会话里（plist 的 `LimitLoadToSessionType = Aqua`，daemon.md）才有图形能力。实测：本机交互会话里 `attrs = 0x2030`，图形与 TTY 两位都在。
4. 预检：`ctx = LAContext::new()`，按生产配置设好：`setTouchIDAuthenticationAllowableReuseDuration(0.0)`；`interactionNotAllowed` 保持 false；`localizedFallbackTitle`、`localizedCancelTitle` 不设，保留系统的"使用密码…"回退。再 `ctx.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication)`：`Err(e)` 且 `e.code()` 是 -5（PasscodeNotSet）→ `Unsupported`（`passcode_not_set`）；是 -1004（NotInteractive）→ `Unsupported`（`not_interactive`）；其他 → `Unsupported`（`cannot_evaluate`，`detail.la_code`）。预检失败时先 `ctx.invalidate()` 再放下它。实测：本机两个策略都能评估，`biometryType = 1`（Touch ID），重用时长缺省就是 0.0。
5. 回复通道：`(tx, rx) = sync_channel::<Reply>(1)`；`block = RcBlock::new(move |success: Bool, error: *mut NSError| { … })`。回复块里只做一件事：把 `success.as_bool()`、错误对象是否为空、它的 `domain()` 是否等于 `LAErrorDomain`、它的 `code()` 拼成一个 `Reply`（`Send` 的纯数据），`tx.try_send` 出去；接收端已放下就丢弃。它不碰 `ctx`，也不调 `canEvaluatePolicy`（LAContext.h：在回复块里调它会死锁）。
6. 评估：在 `objc2::exception::catch` 里调 `ctx.evaluatePolicy_localizedReason_reply(LAPolicy::DeviceOwnerAuthentication, &NSString::from_str(&reason), &block)`。抛了 Objective-C 异常 → `ctx.invalidate()`，`NotConfirmed`（`la_exception`）。这个调用不阻塞，回复在框架的私有队列上送达（LAContext.h）。实测：从主线程和后台线程调都一样，回复都不在主线程上。
7. 等回复：期限是第 6 步时 CLOCK_MONOTONIC 的读数加上弹框期限。按片等待：`rx.recv_timeout(min(1 s, 剩余))`，每片之后重读 CLOCK_MONOTONIC [我的决定]。`recv_timeout` 内部用的 `Instant` 在睡眠时不走（§5.7），只靠它的话，睡眠之前弹出的框醒来之后还会再挂将近一分钟。
   - 收到回复 → 第 8 步。
   - 到期 → `ctx.invalidate()`：进行中的评估以 AppCancel 结束，框消失（LAContext.h）。再等迟到的回复至多 `CANCEL_GRACE`，内容一律不看，到期之后才到的成功也作废 → `NotConfirmed`（`timeout`）。
8. 判定（§5.4 的表）：`success` 为真且没有错误对象 → 第 9 步；其余 → 表里对应的 `NotConfirmed` 或 `Unsupported`，不签发。
9. 签发：`issued = clock.instant()`、`mono = clock.mono_ns()`，即认证刚通过的此刻；`nonce` 取 16 字节 `arc4random_buf`，与登记表里已有的键相同就重取（§5.8）；登记表加锁，清扫过期记录，插入 `Record { action: action.clone(), issued, issued_mono_ns: mono, spent: false }`。登记表的锁中毒 → `NotConfirmed`（`internal`），不签发。返回 `ConfirmToken { action, issued, nonce }`。
10. 收尾（每条出路都做）：`ctx.invalidate()`（对已经 invalidate 过的上下文无效果，LAContext.h），放下 `ctx` 与回复块，释放闸门。第 4 到 10 步包在一个 `autoreleasepool` 里：daemon 的线程长寿，回复里的自动释放对象不该在它上面累积。

`require` 不看 TTY、不看环境变量、不看请求方（C4）。daemon 是 launchd agent，本来就没有 TTY（module-rulings Addendum 1 的 T12 一条：confirm checks nothing about a TTY）。

### 5.3 LocalAuthentication 桥

design §6.9 把绑定方式交给构建者；本文选 objc2 项目生成的 `objc2-local-authentication`（§9）。它把 SDK 头文件逐个翻译过来，函数名就是选择子：

| Objective-C（LAContext.h，SDK 27.0） | Rust（objc2-local-authentication 0.3.2） | 用在 |
| --- | --- | --- |
| `+new` | `LAContext::new() -> Retained<LAContext>` | 每次弹框一个新的，从不复用 |
| `-canEvaluatePolicy:error:` | `canEvaluatePolicy_error(&self, LAPolicy) -> Result<(), Retained<NSError>>` | §5.2 第 4 步、`probe` |
| `-evaluatePolicy:localizedReason:reply:` | `evaluatePolicy_localizedReason_reply(&self, LAPolicy, &NSString, &DynBlock<dyn Fn(Bool, *mut NSError)>)`；安全要求：回复块可以跨线程发送 | 第 6 步 |
| `-invalidate` | `invalidate(&self)` | 第 4、6、7、10 步 |
| `touchIDAuthenticationAllowableReuseDuration`（缺省 0，最大 5 分钟；只管锁屏解锁时的指纹能不能被沿用） | `setTouchIDAuthenticationAllowableReuseDuration(f64)` | 显式设 0（"不缓存"） |
| `interactionNotAllowed` | `setInteractionNotAllowed(bool)` | 生产不设；测试接缝的 `LaOptions::quiet()` 设 true |
| `LAPolicyDeviceOwnerAuthentication`（值 2） | `LAPolicy::DeviceOwnerAuthentication` | Touch ID；没有、没录入或被锁住时直接要登录密码；"使用密码…"按钮切换到密码，不结束评估（LAContext.h） |

不用 `LAPolicyDeviceOwnerAuthenticationWithBiometrics`（值 1）：它只认指纹，没有 Touch ID 的时候（外接键盘、合着盖子）就确认不了，而 design §6.9 要的是"Touch ID，回退登录密码"。

线程：objc2 里的 `LAContext` 不是 `Send`，所以它在 `require` 的调用线程上创建、配置、评估、invalidate、放下，从不跨线程；回复块只往通道里送一个 `Reply`。`Confirmer` 是 `Send + Sync`，因而存不下一个 `LAContext`：上下文不可能在两次弹框之间被留下来复用（§7 的 `token_has_no_clone_copy_or_serde` 把两条性质都钉住）。LAContext.h 要求评估期间一直持有对上下文的强引用，否则上下文释放时评估被取消，所以 `ctx` 活到第 10 步。

框上显示的是 `"<进程名>" is trying to <reason>.`（LAContext.h 对 `localizedReason` 的说明）。daemon 是不带 bundle 的可执行文件，进程名应当显示为 `git-stratad`，由 §7 的人工测试 `manual_prompt_round_trip` 核实。

### 5.4 回复到码的映射

LAError 的码取自 SDK 27.0 的 `LAPublicDefines.h` 与 `LAError.h`：19 个具名常量，其中 4 个是别名，合并之后 15 个不同的值（`-1` 到 `-14`，以及 `-1004`）。策略是 `DeviceOwnerAuthentication`，按头文件的说明其中几个码不会出现；真出现了也照表处理，一律宁可拒绝 [我的决定：表中的归类]。

| 回复 | `Code` | `detail.reason` | 说明 |
| --- | --- | --- | --- |
| success = YES，没有错误对象 | —（签发） | — | §5.2 第 9 步 |
| `AuthenticationFailed` -1 | `NotConfirmed` | `failed` | 凭据不对 |
| `UserCancel` -2 | `NotConfirmed` | `cancelled` | 人点了取消 |
| `UserFallback` -3 | `NotConfirmed` | `cancelled` | 本策略下回退按钮切到密码、不结束评估；真出现了就当作没确认 |
| `SystemCancel` -4 | `NotConfirmed` | `interrupted` | 系统打断（锁屏、切换用户……） |
| `PasscodeNotSet` -5 | `Unsupported` | `passcode_not_set` | 账户没有登录密码：本机没有能确认人的手段 |
| `BiometryNotAvailable`（`TouchIDNotAvailable`）-6、`BiometryNotEnrolled`（`TouchIDNotEnrolled`）-7 | `NotConfirmed` | `unexpected` | 本策略下系统会直接要密码 |
| `BiometryLockout`（`TouchIDLockout`）-8 | `NotConfirmed` | `failed` | 指纹失败次数过多；本策略下会改要密码 |
| `AppCancel` -9 | `NotConfirmed` | 我们到期 invalidate 的是 `timeout`，否则 `interrupted` | — |
| `InvalidContext` -10 | `NotConfirmed` | `unexpected` | 上下文已被 invalidate；每次新建，不该出现（`LaOptions { invalidate_first: true, … }` 专门造它） |
| `CompanionNotAvailable`（`WatchNotAvailable`）-11、`BiometryNotPaired` -12、`BiometryDisconnected` -13、`InvalidDimensions` -14 | `NotConfirmed` | `unexpected` | 与本策略、本用法无关 |
| `NotInteractive` -1004 | `Unsupported` | `not_interactive` | 系统不让显示界面；生产从不设 `interactionNotAllowed`，所以这是环境问题 |
| 其他码；错误对象的 domain 不是 `com.apple.LocalAuthentication`；success = NO 却没有错误对象；success = YES 却带着错误对象 | `NotConfirmed` | `unexpected` | 框架违约或新码，一律拒绝 |
| `evaluatePolicy` 抛 Objective-C 异常 | `NotConfirmed` | `la_exception` | §5.2 第 6 步 |
| 期限已到 | `NotConfirmed` | `timeout` | §5.2 第 7 步；迟到的回复不看 |

`Unsupported` 在 types.md §6.2 里的含义是"没有 GUI 会话，LocalAuthentication 弹不出来"，返回方列着 confirm。`passcode_not_set`、`not_interactive`、`cannot_evaluate` 是同一类"弹不出能确认人的框"，按共同规则映射到这个已有的码，码的集合不变；Task 15 可以把那一行的措辞放宽成"本机做不了主人认证"。`detail.la_code` 给出原始的码，人和 status 都看得到。

### 5.5 `validate(token, expected)`

1. 登记表加锁；锁中毒 → `NotConfirmed`（`internal`）。清扫：删掉 `issued_mono_ns` 早于 `now_mono − RECORD_RETENTION` 的记录。
2. `rec = map.get_mut(&token.nonce)`：没有 → `NotConfirmed`（`unknown`：从没由本签发者签发过，即伪造的或上一个 daemon 进程的，或者早被清扫了）；`rec.spent` → `NotConfirmed`（`used`）。
3. `rec.spent = true`。从这一步起，不论下面哪一条不过，这个 nonce 都已用掉（C2）。
4. `token.action != rec.action` 或 `token.issued != rec.issued` → `NotConfirmed`（`tampered`）。`ConfirmToken` 的字段是 `pub` 的（types.md §4），签发之后改过它的 `action` 或 `issued` 都在这里被发现；年龄也只按登记的 `issued` 算，改 `issued` 延不了寿。
5. `rec.action != *expected` → `NotConfirmed`（`mismatch`，`detail = {reason, expected, confirmed: rec.action}`）。
6. 年龄：`age_i = clock.instant().saturating_duration_since(rec.issued)`，`age_m = now_mono − rec.issued_mono_ns`；`now_mono` 小于 `rec.issued_mono_ns`（时钟倒退，不该发生）当作过期。`max(age_i, age_m) >= TOKEN_TTL` → `NotConfirmed`（`expired`，`detail.age_ms`）。
7. `Ok(())`。

先看 tampered，再看 mismatch，最后看年龄。顺序只决定报哪个 `reason`，结果都是拒绝。复杂度 O(1)，外加一次小清扫 O(n)，n 是 10 分钟内签发的凭据数，由人的手速限定。

### 5.6 框上的文字：`prompt_reason`

系统把它拼进 `"<进程名>" is trying to <reason>.`，所以它是一个小写开头、不带句号的英文动词短语 [我的决定：用英文，要换语言只改这一个函数]。里面只放 `Action` 里的标识，不放任何自由文本：ticket 标签、提交说明这类文字是 agent 写的，放进框里，agent 就能借框说话。

| `Action` | 文字 |
| --- | --- |
| `PushDeploy { repo, branch }` | `push <repo> to origin/<branch>` |
| `DepsApprove { proposal }` | `approve dependency proposal <proposal>` |
| `RemoveDiscard { session }` | `discard the unpublished changes of session <session> and remove it` |
| `ConfigRelax { repo }` | `apply relaxed push settings for <repo>`（`RepoId` 的路径） |

例：`"git-stratad" is trying to push front to origin/main.`

转义：可打印 ASCII（0x20 到 0x7E）原样输出，`\` 写成 `\\`，其余每个字符写成 `\u{XXXX}`（大写十六进制）。`RepoName`、`SessionId`、`ProposalId` 本来就只含安全字符；`branch` 是配置文件里的字符串，git 的 ref 规则挡住了控制字符与空格，却挡不住 U+202E（从右到左改写）这类双向控制符和零宽字符；`RepoId` 是路径。转义之后，框上的文字没法被排版成看起来像另一个动作。不截断：截断的文字就不再逐字说出要做的动作了。

daemon 往框里加不了别的东西：`require` 只收 `Action`。所以 `deps approve` 的 `--sessions` 子集、一次推送具体带上哪些 commit，都不在框上；cli 在发请求之前把它们打在终端上（cli.md）。它们该不该成为确认的一部分，是 types.md 的事（§9 末尾列为给 T15 的问题）。

### 5.7 时钟

- `ConfirmToken.issued` 是 `Instant`（types.md §4："签发时刻（单调时钟）"）。Rust 在 Darwin 上的 `Instant` 是 `clock_gettime(CLOCK_UPTIME_RAW)`（std 的 `Instant` 文档的平台表；本机 rust-src 的 `sys/time/unix.rs` 实读），按 `clock_gettime(3)` 的说明，这个时钟在系统睡眠时不前进。只用它的话，合上盖子一小时，30 秒前签发的凭据醒来还是新的。
- 所以登记表另记一个 `CLOCK_MONOTONIC` 的读数（`clock_gettime(3)`："will continue to increment while the system is asleep"），年龄取两者的较大值 [我的决定]。这个读数只在 confirm 内部，types.md 的 `ConfirmToken` 不变。
- `require` 的弹框期限也按 `CLOCK_MONOTONIC` 算（§5.2 第 7 步）。
- 不用墙上时间（`SystemTime`）：人或 NTP 都能把它往回拨。

### 5.8 nonce

用 `arc4random_buf` 取 16 字节作 `u128`。macOS 的 `arc4random(3)` 是密码学伪随机数发生器，不会失败，没有错误要处理。nonce 是登记表的键，用来认身份，不是秘密：它只在 daemon 内存里，出现在 `Debug` 输出里也无妨（types.md 只让 `ConfirmToken` derive `Debug`）。与登记表里已有的键相同就重取：概率是 2⁻¹²⁸，写出来是为了让"键唯一"不靠概率。

### 5.9 锁与并发

- 两把锁：闸门（`require` 第 2 到 10 步持有）与登记表（`require` 第 9 步、`validate` 全程，都很短）。顺序是先闸门后登记表；`validate` 只拿登记表。弹框期间不持有登记表，所以框开着时 `validate` 照常进行。
- confirm 不知道 daemon 的 repo 写锁与会话队列，§5.1 第 4 步的约定由 daemon 遵守。
- 没有 checkpoint，没有 rename：confirm 不写工作区，也不写任何文件。模板问的"先做 checkpoint、原子替换各在哪一步"，对本模块的回答是都没有。

### 5.10 复杂度与 design §8 的数字

design §8 没有给 confirm 的验收数字：弹框花的是人的时间。confirm 自己的开销：一次图形会话探测，一次预检，各是一次本机调用；实测一次不弹框的评估往返（`interactionNotAllowed = true` → -1004）约 1 ms；`validate` 是 O(1) 加一次小清扫。`require` 最长阻塞 `PROMPT_TIMEOUT + CANCEL_GRACE` = 65 s，外加调度误差。

## 6. 失败与恢复

confirm 不写任何对外可见的东西（§5.9），它的每一个失败都发生在"第一次对外写入之前"，都是具名拒绝；它从不返回 `Unknown`，也不留现场（module-rulings 共同规则的错误时机；C7）。

| 失败 | 在哪一步 | 码（`detail.reason`） | 留下什么 | 怎样恢复 |
| --- | --- | --- | --- | --- |
| 另一个框正开着 | `require` 第 2 步 | `NotConfirmed`（`prompt_in_progress`） | 无；那个框不受影响 | 等它结束再发命令 |
| daemon 不在有图形能力的会话里（在 ssh 登录里手动起的，或被当作系统 daemon 起的） | 第 3 步 | `Unsupported`（`no_gui_session`；`SessionGetInfo` 本身失败是 `session_info`） | 无 | 让 daemon 作为用户 Aqua 会话里的 launchd agent 运行（daemon.md） |
| 账户没有登录密码 | 第 4 步或第 8 步 | `Unsupported`（`passcode_not_set`） | 无 | 设登录密码 |
| 系统不让显示界面；预检的其他失败 | 第 4 步或第 8 步 | `Unsupported`（`not_interactive`、`cannot_evaluate`，带 `la_code`） | 无 | 看 `la_code`；status 里有 `probe` 的结果 |
| 人点了取消、认证失败、指纹被锁 | 第 8 步 | `NotConfirmed`（`cancelled`、`failed`，带 `la_code`） | 无凭据 | 人重新发命令 |
| 系统打断（锁屏、切换用户） | 第 8 步 | `NotConfirmed`（`interrupted`） | 无凭据 | 同上 |
| 没人理睬，期限到了（睡眠时间也算） | 第 7 步 | `NotConfirmed`（`timeout`） | 框已被 invalidate 收走 | 同上 |
| 框架违约、未知的码、Objective-C 异常 | 第 6、8 步 | `NotConfirmed`（`unexpected`、`la_exception`） | 无 | 同上；反复出现说明系统升级改了行为，看 `la_code` |
| 签发登记表的锁中毒（持锁时 panic） | `require` 第 9 步、`validate` 第 1 步 | `NotConfirmed`（`internal`） | 不签发、不核销 | 重启 daemon，之前的凭据一律作废 |
| 凭据没登记过、用过、被改过、动作不符、过期 | `validate` 第 2 到 6 步 | `NotConfirmed`（`unknown`、`used`、`tampered`、`mismatch`、`expired`） | nonce 已核销（`unknown` 无可核销） | 重新确认；`tampered` 与 `mismatch` 只会来自 daemon 的 bug，status 应当报出 |
| daemon 在框开着时崩溃或重启 | — | cli 的连接断开（码由 cli.md 定） | 进程退出，上下文随之释放，框消失；没有凭据 | 重新发命令 |
| daemon 在核销之后、动作完成之前崩溃 | — | 由那个动作的模块定（session、publish 的 `Unknown` 与现场） | 那个动作自己的现场 | 重启后凭据已经不存在，不会被重放（不变量 6）；越过未决状态要新的确认（例如 `rm --discard`，session.md §5.6） |

## 7. 测试清单

矩阵里没有目标模块为 `confirm` 的行。读过的相关行：

- 3d `layerfs-bridge/tests/duplex.rs::wrong_static_identity_fails_the_actual_handshake`（`:337`，目标 daemon，替代）的理由点名 confirm："同 uid 的 unix socket 由文件权限限定访问；需要人类确认的动作走 confirm（LocalAuthentication，design §6.9），与通道认证无关"。通道那一半归 daemon；confirm 这一半是"从同 uid 的 socket 来的请求照样要弹框"，由下表的 `no_env_or_flag_bypass` 承担，它的 RPC 部分就是直接连 socket 的客户端。
- 3d 其余 27 行"无需认证通道"（`:57`、`:58`、`:160`、`:166`、`:253` 到 `:256`、`:258`、`:330` 到 `:336`、`:338` 到 `:342`、`:346` 到 `:351`）把 LayerFS 的认证通道换成同 uid 的 unix socket：26 行归 daemon，`:160` 归 gitio，都不要 confirm 的测试。
- 3d `layerfs-daemon/tests/forced_producers.rs::fp32_unknown_commit_custody_is_forced_only_when_relinquished_and_stays_unknown`（`:144`，目标 session）：越过未决状态删会话只能经 `rm --discard` 并需人类确认。session.md 用 `remove_unresolved_scene_requires_discard_confirmation` 承担它；confirm 这一侧是 `RemoveDiscard` 的签发与校验（下表的 `prompt_reason_names_the_action`、`token_single_use`、`wrong_action_is_refused`）。
- 3e `sdk/js/test/packaging.mjs::dispatch`（`—`，不保留）：理由说最近的对应物是 design §6.9 的"需确认动作不得经无确认通道到达"，但语义不同，不算保留。那个对应物就是 `no_env_or_flag_bypass` 的 RPC 部分；该行仍是不保留。

所以下表除了一行引用 3d `:337`，全是新增：签发、校验、过期、重放、动作不符、取消。计划点名的四个是 `token_single_use`、`token_expires`、`non_tty_refused_before_prompt`、`no_env_or_flag_bypass`。

测试环境：Task 1 到 3 的单元测试用 `test-seams` 的 `ScriptedAuth` 与 `FakeClock`，不碰 LocalAuthentication，弹框期限与 `CANCEL_GRACE` 调到 50 ms。Task 4 用真的 LocalAuthentication，但只走不弹框的路径（`LaOptions::quiet()`、`read_back_la_config`），在没有图形会话的 CI 机器上也能跑；唯一会弹框的 `manual_prompt_round_trip` 带 `#[ignore]`，要人坐在机器前。要设环境变量的测试一律放到子进程里：测试二进制以 `--exact <子测试名>` 重新执行自己，因为 edition 2024 里 `std::env::set_var` 是 `unsafe`，而并行的测试线程共享同一份环境。Task 6 的端到端测试在 cli 的 crate 里跑真的 `git-strata` 二进制，经 daemon 的测试接缝起一个进程内的 daemon，它的 `Confirmer` 用 `ScriptedAuth`；`non_tty_refused_before_prompt` 的脚本全是 `Verdict::pass()`——只要有一条路绕得过框之前的检查，动作就会真的发生，测试一定看得见。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `token_has_no_clone_copy_or_serde` | 静态断言（`static_assertions::assert_not_impl_any!`、`assert_impl_all!`）：`ConfirmToken` 不实现 `Clone`、`Copy`、`Serialize`、`DeserializeOwned`；`Confirmer` 实现 `Send` 与 `Sync`；objc2 的 `LAContext` 不实现 `Send`（所以 `Confirmer` 存不下上下文，每次只能新建，C1）。一个凭据只能按值移走一次，复制、序列化都编译不过 | 新增（types.md §2.2、§4；C5） |
| `prompt_reason_names_the_action` | 四个变体的文字逐字等于 §5.6 的表：`push tool to origin/main`、`approve dependency proposal 12`、`discard the unpublished changes of session s-k3q7xm2a and remove it`、`apply relaxed push settings for /Volumes/GWD/inter/tool`；对任何输入，结果非空、只含可打印 ASCII；`branch` 里的 U+202E、U+200B、换行、`\` 依次输出为 `\u{202E}`、`\u{200B}`、`\u{A}`、`\\`；两个只差 `branch` 的 `PushDeploy` 文字不同 | 新增 |
| `require_pass_mints_a_registered_token` | 脚本 `Verdict::pass()`：`require(PushDeploy { tool, main })` 返回的凭据 `action` 与之相同；`PromptLog::shown` 恰好一条，等于 `prompt_reason` 的结果；`validate(&t, &action)` 为 Ok。同一动作再 `require` 一次：又弹一次框（`shown` 两条），两个 nonce 不同，各自只能核销一次（每个动作各弹一次，不缓存） | 新增（design §6.9） |
| `token_single_use` | 同一凭据第二次 `validate` → `NotConfirmed`，`reason = "used"`；先以错误的 `expected` 核销一次（`mismatch`），再以正确的 `expected` 核销 → `used`：失败的核销也用掉 nonce；`FakeClock::advance(RECORD_RETENTION)` 之后再核销 → `unknown`（记录已清扫）；三种情形都不成功，`record_count` 随清扫回到 0 | 新增（design §6.9 "一次一用"；C2） |
| `token_expires` | 签发后 `advance(29 s)` → Ok；另一个凭据 `advance(30 s)` → `expired`，`detail.age_ms` 不小于 30000；再一个凭据只 `sleep(31 s)`（`Instant` 不动，模拟合盖）→ `expired`；过期的凭据之后再核销 → `used` | 新增（C3、§5.7） |
| `wrong_action_is_refused` | 参数化：`RemoveDiscard { s1 }` 的凭据对 `expected = RemoveDiscard { s2 }`；`PushDeploy { tool, main }` 对 `PushDeploy { tool, release }` 与 `PushDeploy { front, main }`；`DepsApprove { P }` 对 `DepsApprove { Q }`（凭据从不覆盖替换它的提议）；`RemoveDiscard { s1 }` 对 `PushDeploy { tool, main }`。都是 `NotConfirmed`，`reason = "mismatch"`，`detail.expected` 与 `detail.confirmed` 是两个 `Action`；之后以正确的 `expected` 再核销 → `used` | 新增（module-rulings「A confirmation names the exact plan」；C3） |
| `wrong_action_is_refused` 的 `ConfigRelax` 一对 | `ConfigRelax { R }` 的凭据对 `expected = ConfigRelax { S }`（R ≠ S）：`NotConfirmed`，`reason = "mismatch"`，`detail.expected` 与 `detail.confirmed` 是两个 `Action`；之后以正确的 `expected` 再核销 → `used`。与上一行同一个测试、同一条核销。这一对现在可执行 | 新增（module-rulings Addendum 1 的 T15 一条；C3） |
| `forged_or_tampered_token_is_refused` | 手工构造的 `ConfirmToken { action, issued: Instant::now(), nonce: 随机数 }` → `unknown`；另一个 `Confirmer`（模拟 daemon 重启）签发的凭据交给这一个 → `unknown`；签发之后把 `token.action` 改成别的动作，并以新动作作 `expected` → `tampered`，之后原样再交 → `used`；把 `token.issued` 往后挪（想延寿）→ `tampered` | 新增（types.md §4 字段是 pub；C1 到 C3） |
| `user_cancel_mints_nothing` | 脚本依次 `Verdict::la(-2)`、`la(-1)`、`la(-4)`、`la(-8)`：`require` 依次返回 `NotConfirmed`，`reason` 是 `cancelled`、`failed`、`interrupted`、`failed`，`detail.la_code` 是原码；`record_count() == 0`；`shown` 每次加一条；`invalidated()` 等于弹框次数，`cancelled_while_pending() == 0` | 新增（design §6.9） |
| `la_error_mapping_covers_every_code` | 表驱动：对 §5.4 表的每一行（SDK 27.0 的 15 个不同值、未知码 -999、外域错误、success = NO 而没有错误对象、success = YES 却带着错误对象、`Verdict::Throw`）用对应的 `Verdict` 跑 `require`，得到的 (`Code`, `reason`, `la_code`) 逐行等于表；只有 `Verdict::pass()` 签发 | 新增 |
| `prompt_timeout_invalidates_and_mints_nothing` | `Hang`、期限 50 ms：返回 `timeout`，`cancelled_while_pending() == 1`，返回发生在迟到的 AppCancel 收到之后；`PassAfterCancel`：迟到的成功被丢弃，仍是 `timeout`，`record_count() == 0`；`Silent`：等满 `CANCEL_GRACE` 后返回 `timeout`；期限 60 s、另一线程在 100 ms 时 `clock.sleep(61 s)`：`require` 在约 1 s 内返回 `timeout`（期限按睡眠时也走的时钟算） | 新增（§5.2 第 7 步） |
| `concurrent_prompt_is_refused_not_queued` | 线程 A 的 `require` 停在 `Hang`（期限 2 s）；这期间线程 B 的 `require` 立即返回 `NotConfirmed`（`prompt_in_progress`），`shown` 只有 A 那一条，`probes()` 不因 B 增加；框开着时对另一个凭据的 `validate` 照常返回（登记表没被弹框占着）；A 结束后 B 重发，弹出自己的框 | 新增（C6、§5.9） |
| `no_gui_session_is_unsupported_before_prompt` | `ScriptedAuth::new(false, …)`：`require` → `Unsupported`（`no_gui_session`），`shown` 为空，预检没被调用；图形会话在、`can_evaluate` 为 `Err(-5)` → `Unsupported`（`passcode_not_set`）；`Err(-1004)` → `not_interactive`；`Err(-6)` → `cannot_evaluate`，`la_code = -6`。`probe()` 在这四种情形给出同样的码与 `reason`，正常情形是 Ok；都不弹框 | 新增（types.md §6.2 `Unsupported`） |
| `la_context_is_configured_fresh_without_reuse` | 真 LocalAuthentication，不评估：`read_back_la_config()` 读回 `reuse_duration == 0.0`、`interaction_not_allowed == false`、`fallback_title_is_default == true`（保留"使用密码…"回退） | 新增（design §6.9 "不缓存"；C1） |
| `la_non_interactive_round_trip_maps_codes` | 真 LocalAuthentication，永不弹框，`Confirmer::with_la(LaOptions::quiet())`：在有图形会话的机器上 `require` → `Unsupported`（`not_interactive`，`la_code = -1004`）；`invalidate_first = true` → `NotConfirmed`（`unexpected`，`la_code = -10`）；没有图形会话的机器（CI）上两者都是 `Unsupported`（`no_gui_session`），与直接调 `SessionGetInfo` 读到的位一致；`probe()` 与之一致；都在 5 s 内返回，`record_count() == 0`。实测（2026-10-23）：-1004 约 1 ms，-10 不到 1 ms，回复在框架的私有队列上，从后台线程调也一样。[实现时补的决定]（2026-10-26）界限从 1 s 放宽到 5 s：机器负载约 76 时全工作区测试里实测 1.5–1.9 s（单独跑仍是毫秒级）。5 s 仍远小于 60 s 的弹框期限，真弹了框或挂住的回归照样失败 | 新增 |
| `manual_prompt_round_trip` | `#[ignore]`，要人坐在机器前（`cargo test -p strata-confirm --features test-seams --test la_manual -- --ignored --nocapture`），`Confirmer::with_la` 的交互版本、期限 10 s。第一次框用 Touch ID 或密码通过 → 签发，`validate` 成功；第二次点取消 → `cancelled`；第三次不理睬 → `timeout`，框自己消失。人核对框上写着 `… is trying to push tool to origin/main.`。daemon 装好之后，从 launchd agent 里再跑一次（§9 的待定项） | 新增 |
| `only_confirm_constructs_tokens` | 扫描工作区 `crates/*/src/**/*.rs`：`ConfirmToken {` 结构体字面量（不算定义处的 `struct ConfirmToken {`）只出现在 `crates/strata-confirm/src/` 里；测试里构造凭据的代码只放在各 crate 的 `tests/` 目录下（session 的 `remove_discard_checks_the_token` 就是这样），放在 `src/` 里的 `#[cfg(test)]` 模块也算违规 | 新增（types.md §4："除了 strata-confirm 和测试，任何代码都不构造 ConfirmToken"；D1） |
| `confirm_reads_no_environment` | 运行期：`bypass_env_names` 给出的每个名字（工作区源码里出现过的每个 `STRATA_` 名字，并上固定的 `STRATA_CONFIRM`、`STRATA_NO_CONFIRM`、`STRATA_SKIP_CONFIRM`、`STRATA_ASSUME_YES`、`STRATA_YES`、`STRATA_FORCE`、`STRATA_TEST`、`STRATA_CI`、`STRATA_NONINTERACTIVE`、`STRATA_TTY`、`STRATA_DEBUG`、`STRATA_PUBLISH`）分别取 `1`、`true`、`yes`、`on`、`0`、空串，外加全部同时为 `1` 的一次，在 stdin 为 `/dev/null` 的子进程里跑 `require`：脚本是 `Verdict::la(-2)` 时每次恰好弹一次框、得到 `cancelled`；脚本是 `Verdict::pass()` 时每次恰好弹一次框并签发。环境变量既跳不过框，也挡不住框（confirm 不看 TTY）。静态：`crates/strata-confirm/src/` 里没有 `std::env`、`env::var`、`var_os`、`getenv`、`env!(`、`option_env!(`、`debug_assertions`、`cfg(test)`；能换掉认证器的构造只在 `#[cfg(feature = "test-seams")]` 下 | 新增（C4；design §9 不变量 7） |
| `non_tty_refused_before_prompt` | 在 cli 的 crate 里：daemon 的测试接缝起进程内 daemon，脚本全是 `Verdict::pass()`；fixture 有带未发布改动的会话、一条 Pending 提议、一个 `deploy_on_push = true` 且有可发布批次的仓库。对每个确认类命令（`rm --discard`、`deps approve`、对该仓库的 `push` 与带推送的 `publish`；Task 15 之后加上调松配置的那条命令），以 stdin 为 `/dev/null`、stdin 为写着 `y` 加换行的管道各跑一次真的 `git-strata`：stdout 是 `{"ok":false,"error":{"code":"NotTty",…}}`；`PromptLog::shown` 为空；daemon 的测试接缝记下的请求里没有这一条（cli 在连接 daemon 之前就拒绝）；会话还在、提议仍是 Pending、`origin` 的分支没动。对照：同一命令在伪终端（`openpty`）里跑，恰好弹一次框并成功，证明这套 fixture 看得见弹框 | 新增（design §6.9；types.md §6.2 `NotTty`） |
| `no_env_or_flag_bypass` | 同上的 fixture，命令都在伪终端里跑（过了 TTY 检查才轮到 confirm），脚本全是 `Verdict::la(-2)`（人点取消）。三部分：(a) 环境：`bypass_env_names` 的名单与取值，同时设在 cli 子进程与 daemon 里，对每个确认类命令各跑一次；(b) flag：遍历 cli 参数定义里每个子命令的每个参数（cli.md 选定的解析器；若是 clap，用 `CommandFactory::command()` 递归），每次往确认类命令上加一个（要值的给一个合法值），另加一次"该子命令的全部布尔 flag 一起上"；(c) RPC：一个直接连 daemon socket 的 JSON-lines 客户端，对每个需要确认的方法各发一次正常请求，再各带一个多余参数（`confirmed`、`confirm`、`token`、`nonce`、`yes`、`assume_yes`、`force`、`skip_confirm`、`tty`）发一次。每一次的结果只能是用法错误或 `InvalidInput`（`shown` 不变），或者恰好一次弹框之后的 `NotConfirmed`；从没有不弹框就成功的；三项检查（会话、提议、`origin` 分支）都显示动作没发生。不做全组合（命令 × flag × 环境约三万次）：每一部分对其余部分取基线，另加一次"全部一起"，共约千次 | 新增<br>layerfs-bridge/tests/duplex.rs::wrong_static_identity_fails_the_actual_handshake |
| `release_daemon_has_no_test_seams` | `cargo tree -p strata-daemon -e features --edges normal --target aarch64-apple-darwin` 的输出里，`strata-confirm` 没有 `test-seams` feature：发布构建里没有脚本化认证器（C8）。git-stratad 见到 `TEST_SEAMS` 为 true 就拒绝启动，由 daemon.md 的测试覆盖 | 新增 |

## 8. 实现任务（TDD）

前提：`strata-types` 已有 types.md §4 的 `Action`、`ConfirmToken`。Task 1 到 5 只依赖它和 §2 的外部 crate，可以在任何别的模块之前做。Task 6 是端到端测试，要等 daemon.md 与 cli.md 的实现任务做完：它要 daemon 的测试接缝（起一个进程内 daemon，它的 `Confirmer` 用 `ScriptedAuth`，测试读得到 `PromptLog` 和收到的请求）与 cli 的二进制和参数定义。任务按顺序做，后面的任务用前面的产出。

### Task 1: 类型性质与框上的文字

**Files:**
- Create: `crates/strata-confirm/Cargo.toml`
- Create: `crates/strata-confirm/src/lib.rs`
- Create: `crates/strata-confirm/src/reason.rs`
- Test: `crates/strata-confirm/tests/token_type.rs`
- Test: `crates/strata-confirm/tests/prompt_reason.rs`

**Interfaces:**
- Consumes: `strata_types::{Action, ConfirmToken, RepoName, SessionId, ProposalId, StrataError}`；`objc2_local_authentication::LAContext`（只为静态断言）
- Produces: `Result`、`TOKEN_TTL`、`PROMPT_TIMEOUT`、`CANCEL_GRACE`、`RECORD_RETENTION`、`TEST_SEAMS`、`prompt_reason`；`Confirmer` 结构体（§4.1 的字段，方法在 Task 2 加）

- [ ] **Step 1: 写失败的测试**：`token_has_no_clone_copy_or_serde`、`prompt_reason_names_the_action`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-confirm --test token_type --test prompt_reason`
Expected: FAIL（`strata-confirm` 还没有 `prompt_reason` 与 `Confirmer`）

- [ ] **Step 3: 写让测试通过的最少实现**：Cargo.toml 只列 §2 的依赖，feature `test-seams = []`；§5.6 的文字与转义，`match` 不写通配分支；`Confirmer` 的字段里装 `Mutex` 与 trait 对象，使它是 `Send + Sync`
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-confirm --test token_type --test prompt_reason`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-confirm/Cargo.toml crates/strata-confirm/src/lib.rs crates/strata-confirm/src/reason.rs crates/strata-confirm/tests/token_type.rs crates/strata-confirm/tests/prompt_reason.rs
git commit -m "feat(confirm): token type guarantees and prompt text"
```

### Task 2: 签发、登记与核销

**Files:**
- Create: `crates/strata-confirm/src/registry.rs`
- Create: `crates/strata-confirm/src/clock.rs`
- Create: `crates/strata-confirm/src/confirmer.rs`
- Create: `crates/strata-confirm/src/seams.rs`
- Modify: `crates/strata-confirm/src/lib.rs`
- Test: `crates/strata-confirm/tests/mint_validate.rs`

**Interfaces:**
- Consumes: Task 1；`libc::{clock_gettime, CLOCK_MONOTONIC, arc4random_buf}`
- Produces: `Confirmer::require`（只有成功路径：闸门、调认证器、按期限等回复、签发）、`Confirmer::validate`；crate 私有的 `Authenticator`；`seams` 里的 `Verdict`、`ScriptedAuth`、`PromptLog`、`FakeClock`、`Confirmer::with_seams`、`Confirmer::record_count`

- [ ] **Step 1: 写失败的测试**：`require_pass_mints_a_registered_token`、`token_single_use`、`token_expires`、`wrong_action_is_refused`、`forged_or_tampered_token_is_refused`。`wrong_action_is_refused` 含 `ConfigRelax` 那一对（§7），这一步就写、就跑。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-confirm --features test-seams --test mint_validate`
Expected: FAIL（`Confirmer` 还没有 `require`、`validate`，`seams` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5 的全部步骤；§5.7 的两个时钟（`FakeClock` 是加了偏移的真时钟）；§5.8 的 nonce；`require` 的第 1、5、7（按片等待，到期只返回 `timeout`）、8（只认成功，其余一律 `unexpected`）、9、10 步；`RECORD_RETENTION` 的清扫
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-confirm --features test-seams --test mint_validate`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-confirm/src/registry.rs crates/strata-confirm/src/clock.rs crates/strata-confirm/src/confirmer.rs crates/strata-confirm/src/seams.rs crates/strata-confirm/src/lib.rs crates/strata-confirm/tests/mint_validate.rs
git commit -m "feat(confirm): mint, register and redeem single-use tokens"
```

### Task 3: `require` 的拒绝路径

**Files:**
- Create: `crates/strata-confirm/src/outcome.rs`
- Modify: `crates/strata-confirm/src/confirmer.rs`
- Modify: `crates/strata-confirm/src/seams.rs`
- Test: `crates/strata-confirm/tests/require_failures.rs`

**Interfaces:**
- Consumes: Task 2
- Produces: §5.4 的映射；`require` 的第 2、3、4、6、7 步全文（闸门拒绝、图形会话、预检、异常、到期 invalidate 与 `CANCEL_GRACE`）；`Confirmer::probe`

- [ ] **Step 1: 写失败的测试**：`user_cancel_mints_nothing`、`la_error_mapping_covers_every_code`、`prompt_timeout_invalidates_and_mints_nothing`、`concurrent_prompt_is_refused_not_queued`、`no_gui_session_is_unsupported_before_prompt`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-confirm --features test-seams --test require_failures`
Expected: FAIL（Task 2 把所有失败都报成 `unexpected`，不 invalidate、不拒绝并发、不探测图形会话，`probe` 不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.4 的表写成一个纯函数（输入回复与"是不是我们到期 invalidate 的"），表驱动测试直接覆盖它；闸门用 `try_lock`；到期先 invalidate，再等迟到的回复至多 `CANCEL_GRACE`；`probe` 复用第 3、4 步
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-confirm --features test-seams --test require_failures`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-confirm/src/outcome.rs crates/strata-confirm/src/confirmer.rs crates/strata-confirm/src/seams.rs crates/strata-confirm/tests/require_failures.rs
git commit -m "feat(confirm): refuse cancelled, timed-out, concurrent and unsupported prompts"
```

### Task 4: LocalAuthentication 桥

**Files:**
- Create: `crates/strata-confirm/src/la.rs`
- Modify: `crates/strata-confirm/src/confirmer.rs`
- Modify: `crates/strata-confirm/src/seams.rs`
- Test: `crates/strata-confirm/tests/la_bridge.rs`
- Test: `crates/strata-confirm/tests/la_manual.rs`

**Interfaces:**
- Consumes: Task 3；`objc2_local_authentication::{LAContext, LAPolicy, LAError, LAErrorDomain}`、`objc2::{exception::catch, rc::autoreleasepool, runtime::Bool}`、`block2::RcBlock`、`objc2_foundation::NSString`、`objc2_security::{SessionGetInfo, callerSecuritySession, SessionAttributeBits}`
- Produces: `LocalAuth`（`Authenticator` 的生产实现）；`Confirmer::new`；`seams` 里的 `LaOptions`、`LaConfigReadback`、`read_back_la_config`、`Confirmer::with_la`

- [ ] **Step 1: 写失败的测试**：`la_context_is_configured_fresh_without_reuse`、`la_non_interactive_round_trip_maps_codes`、`manual_prompt_round_trip`（`#[ignore]`）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-confirm --features test-seams --test la_bridge`
Expected: FAIL（`LocalAuth`、`Confirmer::with_la`、`read_back_la_config` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3 的表逐项照做；上下文只在 `require` 的调用线程上活；回复块只读 `Bool` 与错误对象的 `domain()`、`code()`，经通道送出；评估包在 `catch` 里，第 4 到 10 步包在 `autoreleasepool` 里
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-confirm --features test-seams --test la_bridge`
Expected: PASS（`manual_prompt_round_trip` 被忽略；有人在场时另跑 `cargo test -p strata-confirm --features test-seams --test la_manual -- --ignored --nocapture`，三个框依次通过、取消、不理睬，结果与 §7 一致）

- [ ] **Step 5: Commit**

```bash
git add crates/strata-confirm/src/la.rs crates/strata-confirm/src/confirmer.rs crates/strata-confirm/src/seams.rs crates/strata-confirm/tests/la_bridge.rs crates/strata-confirm/tests/la_manual.rs
git commit -m "feat(confirm): LocalAuthentication bridge through objc2"
```

### Task 5: 绕过防线：凭据只在 confirm 里造，环境变量什么都改不了

**Files:**
- Modify: `crates/strata-confirm/src/seams.rs`
- Test: `crates/strata-confirm/tests/guards.rs`

**Interfaces:**
- Consumes: Task 4；开发依赖 `regex`
- Produces: `seams::bypass_env_names`

- [ ] **Step 1: 写失败的测试**：`only_confirm_constructs_tokens`、`confirm_reads_no_environment`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-confirm --features test-seams --test guards`
Expected: FAIL（`bypass_env_names` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：`bypass_env_names` 扫 `<workspace_root>/crates` 下的 `.rs` 文件，取 `STRATA_[A-Z0-9_]+`，并上 §7 的固定名单，排序去重；工作区根由 `CARGO_MANIFEST_DIR` 的上两级得到（在测试里，不在 `src` 里）；子进程用 `--exact` 重新执行测试二进制，环境变量经 `Command::env` 设，stdin 接 `/dev/null`；源码扫描找到违规时，失败信息列出文件与行号
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-confirm --features test-seams --test guards`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-confirm/src/seams.rs crates/strata-confirm/tests/guards.rs
git commit -m "test(confirm): tokens are built only by confirm and no env var reaches it"
```

### Task 6: 端到端：没有 TTY 先拒，任何 flag、环境变量和 RPC 参数都绕不过（在 daemon.md、cli.md 的实现任务之后）

**Files:**
- Modify: `crates/git-strata/Cargo.toml`（开发依赖 `strata-daemon` 打开 `test-seams`，它再打开 `strata-confirm/test-seams`；git-strata 对 strata-confirm 不加依赖，开发依赖也不加，types.md §8）
- Test: `crates/git-strata/tests/confirm_gate.rs`

**Interfaces:**
- Consumes: daemon.md 的测试接缝（起进程内 daemon 并给它的 `Confirmer` 装上 `ScriptedAuth`、读 `PromptLog` 与收到的请求；名字由 daemon.md 定，它经 daemon 再导出 confirm 的 `seams::{Verdict, PromptLog, bypass_env_names}`）；cli 的二进制（`CARGO_BIN_EXE_git-strata`）与参数定义；`libc::openpty`
- Produces: 只有测试

- [ ] **Step 1: 写失败的测试**：`non_tty_refused_before_prompt`、`no_env_or_flag_bypass`、`release_daemon_has_no_test_seams`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p git-strata --test confirm_gate`
Expected: FAIL（`confirm_gate.rs` 里的 fixture 与断言是新写的：在它们全部写完、对着真 daemon 跑通之前，至少"对照组恰好弹一次框"那一条不成立）

- [ ] **Step 3: 写让测试通过的最少实现**：只写测试的 fixture（会话、Pending 提议、`deploy_on_push` 仓库与可发布批次、伪终端）；任何一条断言不过，说明 daemon 或 cli 有一条绕过确认的路，修那里，不改断言
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p git-strata --test confirm_gate`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/git-strata/Cargo.toml crates/git-strata/tests/confirm_gate.rs
git commit -m "test(confirm): end-to-end, no TTY, flag, env or RPC path skips the prompt"
```

## 9. 构建者决定记录

| 事项（design 里标 [交给构建者]、落在本模块） | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| LocalAuthentication 的绑定方式（design §6.9"[实现：objc2 的 LocalAuthentication 绑定，交给构建者]"；§11 第 2 条） | `objc2-local-authentication` 0.3.2（objc2 项目从 SDK 头文件生成的绑定）+ `block2` 的回复块 + `objc2-foundation`；图形会话检查用 `objc2-security` 0.3.2 的 `SessionGetInfo`；不写 Swift 或 Objective-C 胶水 | 与 design §11 第 2 条一致；函数逐个对应 LAContext.h（§5.3 的表），构建不需要 swiftc 或额外的 build.rs；回复块、线程与异常都能在 Rust 里写清楚 | 2026-10-23。待定：从装好的 launchd agent 弹框时，框是否出现在最前面、是否显示 `git-stratad` 这个名字，要在 daemon 装好之后用 `manual_prompt_round_trip` 在 agent 里跑一次确认（spike） |
| 凭据有效期 | 30 s（计划 Task 12 给定），年龄取 `Instant` 与 `CLOCK_MONOTONIC` 两者的较大值 | 数值由计划给定；Darwin 的 `Instant` 睡眠时不走，只用它会让合盖之后的旧凭据仍然有效（§5.7） | 2026-10-23 |

本文其余标 [我的决定] 的地方，按出现的顺序（design、types.md 与裁定都没有定）：

| 位置 | 决定 | 理由 |
| --- | --- | --- |
| §3 `Confirmer` | 签发与校验是 daemon 持有的一个对象上的方法，不是自由函数 | 一次一用要靠登记表；对象比全局 static 好测，重启前后的凭据互不承认 |
| §3 `probe` | 不弹框的探测，给 status 用 | 人在发确认类命令之前就该知道通道能不能用 |
| §3 `prompt_reason` 的 match | 不写通配分支 | 缺 `ConfigRelax` 那一臂就编译不过，逼着框上的文字写出来 |
| §4.2 C6 | 框开着时来的第二个请求立即拒绝，不排队 | 刷请求的客户端排不出一串框；人不会在一串框里顺手点过一个不是自己要的 |
| §4.1 `RECORD_RETENTION` | 10 分钟；核销过的记录留着，只标记已用 | 让用过、过期的凭据报得出 `used`、`expired`；记录很小，签发速度由人限定 |
| §5.1 调用约定 | 先做不需确认的具名拒绝；`require` 不在任何 repo 写锁或会话队列位置里调；签发之后立刻核销；拿锁之后重新推出 `Action` 再核对 | 弹框可以等 60 s，比写锁的 30 s 有界等待长；注定失败的请求不该打扰人；锁外与锁里解析出的选择器可能不同 |
| §5.2 第 3 步 | 弹框之前用 `SessionGetInfo` 查图形能力，没有就 `Unsupported` | 不在没有图形会话的地方调交互式评估；types.md 把"没有 GUI 会话"记在 confirm 的 `Unsupported` 名下 |
| §5.2 第 4 步、§5.4 | 预检失败、`PasscodeNotSet`、`NotInteractive` 映射到 `Unsupported`，其余失败映射到 `NotConfirmed`，未知的码一律拒绝 | 前者是环境做不了，要人修环境；后者是这一次没确认，人重试即可 |
| §5.2 第 6 步 | 评估包在 `objc2::exception::catch` 里，异常 → `NotConfirmed`（`la_exception`） | 长寿的 daemon 不该因一个 Objective-C 异常整个退出 |
| §5.2 第 7 步 | 弹框期限 60 s，按睡眠时也走的时钟、以 1 s 的片检查；`CANCEL_GRACE` 5 s；到期之后才到的成功作废 | 足够按指纹或输入密码；睡眠前弹出的框醒来不该还挂着；期限必须是硬的 |
| §5.2 第 10 步 | 每次弹框包在 `autoreleasepool` 里 | daemon 的线程长寿，自动释放对象不该累积 |
| §5.5 | 只要找到 nonce，任何一次 `validate` 都核销它；先看 tampered，再看 mismatch，最后看年龄 | 一次一用不留试探的余地 |
| §5.6 | 框上的文字用英文、只放标识、逐字符转义、不截断 | 不让 agent 写的文字进框；双向控制符改写不了框上的意思；截断就不再逐字说出动作 |
| §5.7 | 登记表另记 `CLOCK_MONOTONIC`，不用墙上时间 | 睡眠也算进年龄；墙上时间可以被拨回 |
| §5.8 | nonce 用 `arc4random_buf`，与现有键冲突就重取 | 不会失败的密码学随机数；键唯一不靠概率 |
| §7 测试布局 | 构造凭据的测试代码只放在 `tests/` 下；改环境变量的测试在子进程里跑；两个端到端测试放在 cli 的 crate 里 | 扫描规则简单可靠；edition 2024 的 `set_var` 是 `unsafe`；TTY 与 flag 只在 cli 那一层存在 |

需要别的文档处理的地方：

- **daemon.md（T13）**：采用 §5.1 的调用约定（或在它的 §2 写明要改的地方）；plist 把 agent 加载在 Aqua 会话里；`git-stratad` 的 main 见到 `TEST_SEAMS` 为 true 就拒绝启动；测试接缝能起进程内 daemon、给它的 `Confirmer` 装 `ScriptedAuth`、读 `PromptLog` 与收到的请求（§8 Task 6）；RPC 参数类型拒绝未知字段（`no_env_or_flag_bypass` 的 RPC 部分）；status 显示 `probe()` 的结果；`ConfigRelax` 由人发出的一条显式命令触发，不在发现配置调松时自己弹框（否则改一下配置文件就能弹出一串框）；调紧立即生效，不弹框、不签发；那条命令走 §3 的签发，daemon 在写入调松后的钉住值之前消费凭据。
- **cli.md（T14）**：`require_tty` 在连接 daemon 之前做；RPC 等待上限大于 65 s；弹框之前把框上没有的东西打在终端上（`deps approve` 要移动哪些会话、推送带上哪些 commit、替换提议的计划）；参数定义能被测试遍历；§7 的 `non_tty_refused_before_prompt` 与 cli.md 自己的 `confirm_commands_refuse_non_tty` 可以放在同一个测试文件里，前者多断言"一次弹框都没有、请求没发出"。
- **publish.md（T10）**：`PushDeploy` 不带 commit，推送要推的必须就是人确认时看到的那一段（§5.1）；弹框不在 repo 写锁里；`push` 按值收下凭据。
- **types.md（T15）**：`Action::ConfigRelax { repo: RepoId }` 与 `HeadMoved` 的 `main_moved` 含义已在 types.md。§7 的 `ConfigRelax` 一对可执行。仍开放的：§6.2 的 `Unsupported` 一行放宽成"本机做不了主人认证"；考虑 `DepsApprove` 是否要带会话子集、`PushDeploy` 是否要带 commit，好让"确认的就是要做的"（module-rulings「A confirmation names the exact plan」）也覆盖它们；`NotConfirmed` 的 `detail.reason` 词汇与跨模块的 reason 拼写约定（本文用 snake_case）。confirm 不另造码。
