# 通用 Skill 记忆

本文件用于记录本工作区的通用协作约定和可复用 skill。每次在本工作区开启新的 Codex 对话或处理任务时，先阅读本文件，再开始具体工作。

## 启动约定

- 先查看本文件，确认是否有适用于当前任务的通用 skill 或偏好。
- `rlucene/AGENTS.md` 是这些规则的唯一真实来源；当前 Mac 工作区的
  `../AGENTS.md` 只是指向本文件的相对符号链接。以后只修改本文件，不再
  维护第二份副本。
- 同一连续任务或同一讨论脉络中，已经读取过本文件后，后续小问题、追问或澄清不需要反复重新读取；只有开启新的任务、上下文可能中断/压缩后不确定是否仍保留本文件内容，或用户明确要求时，才再次读取。
- 处理本工作区任务时，除了先读取本文件，如果任务涉及 `rlucene`、Lucene 迁移、历史实现决策、已迁移测试，或用户提到“上次/继续/记忆”，应先做 Codex 记忆 quick pass，搜索相关关键词，再开始代码探索或修改。
- 记忆用于快速定位历史结论、代码路径、已知 TODO 和验证方式；如果记忆可能过期，仍需以当前仓库代码和 Java Lucene 源码为准。
- 如果用户在对话中明确补充了可长期复用的做事方式、命名习惯、检查清单或项目约定，应在用户允许后更新到本文件。
- 用户明确要求新增 TODO 时，统一使用 `TODO IMPORTANT` 标记，不使用普通 `TODO`；不因此批量改写既有 TODO。
- 遇到与具体仓库、模块或任务说明冲突的情况，以更具体、更新的说明为准。
- 用户只要求提交或推送 Git 变更时，直接使用本地 `git` 命令完成，不要求安装或认证 GitHub CLI `gh`；只有用户明确要求创建或操作 GitHub PR、Issue，或任务确实需要 GitHub API 时才使用 `gh` 或 GitHub connector。
- 以后修复 bug 时，先检查相关仓库的 `main` 分支工作树：如果 `main` 没有未提交改动，直接在 `main` 上修改；如果 `main` 存在未提交改动，则从相关仓库当前最新已提交的 commit（`HEAD`）创建独立分支，并优先使用独立 worktree 承载修复，不要把 `main` 工作树已有的未提交改动带入修复分支。
- 用户要求“提交代码”或“推送”时，默认包含完整的检查、提交和双 remote 推送流程：先在相关 Rust 仓库执行 `cargo tidy`，确认没有任何编译告警或错误；只有检查通过后，才提交当前变更，并将当前分支同时推送到 `origin` 和 `upstream`。如果 `cargo tidy`、编译检查或任一前置步骤失败，则不得执行提交或推送，并应先报告失败原因。
- Rust 构建或测试因磁盘空间不足而无法继续时，可以先在当前相关仓库或独立 worktree 执行 `cargo clean` 清理构建产物，再继续编译或验证；有其他任务并行运行时，优先只清理本任务使用的 `target`，避免干扰其他任务。

## 通用 Skill

### Java Lucene 到 Rust Lucene 迁移

- 适用场景：在本工作区处理 `rlucene` 或 Lucene 相关任务时。
- 工作背景：工作区包含 `lucene` 和 `rlucene` 两个主要目录；主要目标是将 Java Lucene 的代码迁移/改写为 Rust Lucene。
- `rlucene` 测试目录分层约定：
  - `test_framework` 是共享测试框架和测试辅助代码目录，不是集中放实际测试用例的目录。这里放跨测试模块复用的 mock、base test case、随机化工具、目录/索引辅助类型，以及生产源码在 `#[cfg(test)]` 下需要引用的测试专用类型，例如测试用 `MergePolicy`、`MergeScheduler`、`MockAnalyzer`、`MockDirectoryWrapper` 等。`src` 源码和 `src/test` 如需引用这些共享辅助代码，统一使用 `crate::test_framework::...`，不要再使用 `crate::test::support::...` 或 `crate::test::test_framework::...` 这类绕路别名。
  - 如果某个辅助类型是某个 Java 测试类专用、但需要被生产源码的 `#[cfg(test)]` enum 或其他测试编译路径引用，应在 `test_framework/core/index/` 下创建与对应测试类同名的 snake_case 文件，而不是放进通用 helper 文件。例如 `TestIndexWriterMergePolicy` 专用的 `LatchedSerialMergeScheduler` 应放在 `test_framework/core/index/test_index_writer_merge_policy.rs`。这类文件中同样保留 `#[allow(dead_code)] // for quick search` 的空结构体（如 `struct TestIndexWriterMergePolicy;`），方便在 IDEA/RustRover 中按 Java 测试类名快速定位。
  - `src/test` 是随 `src/lib.rs` 的 `#[cfg(test)] pub mod test;` 编译进 lib-test 二进制的源码内白盒单元测试目录。这里放需要靠近生产源码、需要访问 `pub(crate)`/源码内部测试入口、或需要保留 RustRover 绿色按钮单测/调试体验的实际 `#[test]` 用例。目录结构应尽量镜像生产源码层级。`src/test` 可以依赖 `crate::test_framework::...`；`src/test` 内部测试模块之间的少量互引可使用 `crate::test::core::...` 等 `src/test` 自身路径，但可复用辅助定义不应留在这里，应迁入 `test_framework`。
  - 生产源码中的 `#[cfg(test)]` import 如果依赖测试辅助代码，应只依赖 `crate::test_framework::...`。
- 操作步骤：
  - 排查测试失败或其他 bug 时，第一步先找到对应的 Java Lucene 测试，逐段核对 Rust 测试代码、测试配置、随机化逻辑、并发时序和异常处理是否与 Java 原测试一致；确认测试迁移 fidelity 后，再分析或修改实现代码。
  - Rust 随机化测试应由顶层测试入口创建可复现的 `random`，所有会消费同一测试随机序列的 `do_test`、helper 等方法都必须通过参数接收并继续传递该 `random`（通常为 `&mut StdRng`），不能在方法内部再次调用 `random()` 创建独立随机源，否则测试框架记录的 seed 无法完整复现失败。确实需要独立随机流时，也应从传入的 `random` 派生 seed。
  - Java 测试类中的多个独立测试方法迁移到 Rust 后必须继续保留各自独立的 `#[test]` 入口；不得为了复用 `@BeforeClass`、class-level `setUp` 或其他共享测试数据，把多个测试方法合并进一个聚合测试方法。nextest 的逐测试进程隔离导致共享初始化重复时，也不能用合并测试入口规避；应寻找不改变测试粒度的运行器配置或共享 fixture 方案，无法实现时先说明限制并征求用户确认。
  - 先在 `lucene` 中找到对应 Java 实现，理解原始类、方法、数据结构、边界条件和测试。
  - 再在 `rlucene` 中寻找对应 Rust 模块，沿用现有 Rust 项目的结构、命名和错误处理风格。
  - 迁移时优先保持 Java Lucene 的逻辑、语义、边界行为和测试预期高度一致；只有在 Rust 语言模型要求时才做必要的表达方式调整。
  - Java 类中有几个方法，Rust 迁移时原则上也保持对应的方法结构；即使多个方法之间存在重复代码，也不要自行抽取 helper、工具函数、宏或额外抽象来去除冗余。
  - 转换 Java 源码到 Rust 时，不要因为 Rust 泛型、借用、类型表达或临时编译便利而自行定义 Java 中不存在的 helper 方法/函数/类型；这会增加 Java 与 Rust 对照阅读成本。只有 Rust 语言模型确实无法按 Java 方法结构直接表达时，才先说明原因并征求确认。
  - 转换出的 Rust 方法、结构体、枚举、trait、impl、常量和内部辅助类型的声明先后顺序，原则上要与对应 Java 源文件保持一致；只有 Rust 语法、可见性或模块边界确实要求时才调整位置，并尽量保留 Java 对应项之间的相对顺序。
  - 源码转换时优先使用泛型和静态分发来保持性能与类型信息，不为了省事使用 `dyn`、`Any` 或类型擦除；只有当前 Rust API 边界确实无法静态表达时，才先说明原因并征求确认。
  - 迁移或审计 Java 并发字段对应的 Rust 原子类型时，必须先对照 Java 字段声明和每个具体访问方法，再选择 `Ordering`；不能仅根据 Rust 的 `AtomicBool`、`AtomicUsize` 等类型统一决定，也不能为了风格一致全局批量替换。
  - Java `volatile` 字段，以及 Java `Atomic*` 的默认 `get`、`set`、`getAnd*`、`incrementAndGet`、`compareAndSet` 等具有 volatile 内存效果的方法，Rust 默认使用 `Ordering::SeqCst` 作为 JMM 语义保真基线；`compare_exchange` 的成功和失败 Ordering 都要分别对应 Java 读写语义，失败 Ordering 不能使用 `Release` 或 `AcqRel`。
  - Java 明确使用较弱访问方法时按其语义映射：`getOpaque` / `setOpaque` 对应 `Relaxed`，`getAcquire` 对应 `Acquire`，`setRelease` / `lazySet` 对应 `Release`；其他弱 CAS 或 exchange 变体也必须逐个对照，不能套用默认 `Atomic*` 的 `SeqCst` 规则。例如 `BaseCompositeReader.numDocs()` 的 `getOpaque()` 应使用 `Relaxed` 读取，而普通 `set()` 仍使用 `SeqCst` 写入。
  - Java 侧是普通非 `volatile` 字段，Rust 仅为了共享类型、`&self` 内部可变性或测试适配而改成 atomic 时，使用 `Relaxed`，并继续依赖 Java 原有的外层锁、线程 `join`、对象安全发布或单线程约束；如果 Rust 侧已不具备同等外层同步，则应判定为迁移设计问题，不能只靠提高 Ordering 掩盖。
  - Java `LongAdder` / `LongAccumulator` 一类本身只提供弱快照聚合语义，或 Rust-only atomic 只承担独立统计、唯一 ID、诊断值且不发布其他内存时，可以使用 `Relaxed`。
  - Rust 为替代 Java `Semaphore`、`CountDownLatch`、资源发布标志等同步原语而新增 atomic 时，应按明确的 happens-before 协议成对使用 `Release` 写入和 `Acquire` 读取；同时读取旧值并发布新状态的 RMW 使用 `AcqRel`。只有确实要求所有相关原子操作存在单一全序时才使用 `SeqCst`。
  - Rust-only 的“一次性调用”或“唯一获胜者”标志，如果只用于决定哪个线程成功、不负责发布其他内存，使用 `Relaxed` 的 `compare_exchange` / `swap` 即可。不能写成 `load` 检查后再 `store`，因为再强的 Ordering 也无法阻止多个线程同时通过检查；应改为单个原子 RMW。
  - `SeqCst` 不能让多个独立 atomic 自动形成一致快照，也不能修复 check-then-act 竞态；需要跨字段不变量时使用同一把锁、版本号/seqlock 或其他明确协议。Ordering 的正确性必须结合原子字段所保护或发布的其他数据一起说明。
  - 源码和单元测试中的注释也必须同步转换；不要随意删除、增补或改写注释含义。
  - 转换注释时保持原注释的语义和信息量，但要使用 Rust 注释风格和 Rust 对应术语；例如 JavaDoc 转为 Rust doc comment，`class`、`sub-class` 等 Java 语言术语要按实际 Rust 结构转换为 `struct`、`trait`、`impl`、实现类型等对应表达。
  - 对复杂算法、位运算、索引格式、排序/比较规则、编码解码逻辑、异常/错误路径，要逐段对照 Java 源码，避免凭印象重写。
  - 涉及普通基础类型固定字节宽度的计算时，例如 Java `Integer.BYTES`、`Long.BYTES`、`Short.BYTES`、`Float.BYTES`、`Double.BYTES` 对应的 Rust 代码，优先使用 `BitUtil` 中已有的 `INT_BYTES`、`LONG_BYTES`、`SHORT_BYTES`、`FLOAT_BYTES`、`DOUBLE_BYTES` 等常量或相关方法；不要直接写 `std::mem::size_of::<i32>()` 这类表达。只有 `BitUtil` 没有对应项，或语义确实是 Rust 布局/inline size 估算时，才使用 `std::mem::size_of` / `size_of_val`。
  - Java `try (...) { ... }` / try-with-resources 迁移到 Rust 时，不能只依赖作用域结束或 `Drop` 来关闭资源；需要在对应控制流中显式调用 `close()`，并正确传播 `close()` 产生的错误。若 `try` 主体和 `close()` 都失败，应保留主体错误为主错误，并将 `close()` 错误按 Java suppressed exception 语义追加到主错误上；若主体成功但 `close()` 失败，则返回 `close()` 错误。多个资源必须按 Java 语义以创建顺序的反向关闭；如果后续资源初始化失败，已经初始化成功的前序资源也必须显式关闭，并把关闭错误 suppressed 到初始化错误上。`Drop` 只能作为兜底清理，不能承担需要向调用方返回错误的语义，例如 checksum 校验、footer 校验、临时文件关闭失败等。
  - 以后对比 Java 与 Rust 代码时，如果发现 Java 侧是 `try-with-resources` 或明确调用 `close()` 的资源生命周期，而 Rust 侧没有在对应控制流中显式调用 `close()`、只是依赖作用域结束或 `Drop`，应直接判定为问题并修正。
  - 如果 Java 类型本身实现或继承了 `Closeable` / `AutoCloseable`，Rust 对应 trait / 类型也必须体现 `Closeable` 约束并提供 `close()` 实现；如果 Rust 侧遗漏了这个 super trait 或 close API，应直接判定为迁移错误并补齐，不能因为当前 Rust API 没有 `close()` 就跳过 try-with-resources 的显式关闭语义。
  - 迁移 Java `IOUtils.close(a, b, ...)` 时，如果 Rust 侧多个 closeable 是相同具体类型，应优先直接使用 `IOUtils::close([&mut a, &mut b, ...], Closeable::close)` 表达 Java 的“全部尝试关闭并 suppress 后续错误”语义；只有类型不同、所有权或借用关系无法直接组成同类型集合时，才手动用 `IOUtils::use_or_suppress` 聚合错误。
  - Java 普通 `try { ... } finally { close(); }` 迁移到 Rust 时，不应套用 try-with-resources 的 suppressed exception 语义。必须先保存 `try` 主体结果，再无条件执行 `finally` 中的 `close()` / 清理逻辑；如果 `try` 主体和 `finally` 都失败，普通 Java `finally` 中抛出的错误会覆盖并替代 `try` 主体错误，因此 Rust 也应优先返回 `finally` 错误。只有 `finally` 成功时，才返回 `try` 主体错误。若 Java 代码只是调用 `close()` 而没有把字段置空，Rust 侧也不要为了“已关闭”而额外 `take()` / 清空 `Option`，除非当前 Rust 所有权或后续状态语义确实需要。
  - Java `try/finally` 所覆盖的路径如果可能触发被迁移成 Rust panic 的 Java `Error`，Rust 必须通过 `catch_unwind` 或已有 RAII 机制保证 `finally` 一定执行；涉及可失败清理时优先显式使用 `catch_unwind + Result`，并保持 `finally` 错误覆盖主体错误的 Java 语义。
  - Java 普通 `finally` 块内如果有多条可能失败的语句，Rust 迁移时应把整个 `finally` 块按源码顺序封装成一个 `Result` 流程，而不是把各清理步骤并列聚合。例如 `finally { IOUtils.close(reader, writer); IOUtils.deleteFiles(...); }` 中，如果 `IOUtils.close(reader, writer)` 抛错，Java 会立即退出 `finally`，不会再执行 `IOUtils.deleteFiles(...)`；Rust 也应让前一条清理语句的错误短路后续清理语句。整个 `finally` 块成功后才返回 `try` 主体结果；若 `finally` 块任一语句失败，则返回该 `finally` 错误并覆盖 `try` 主体错误。
  - 如果发现 Java 和 Rust 现有实现不一致，先指出差异，再按 Java Lucene 行为修正或请用户确认。
  - 迁移 `RamUsageEstimator` 或相关内存占用计算时，不要求 Rust 输出与 Java 输出一致，因为 Java/JVM 对象头、数组头、引用压缩和对齐模型与 Rust 不同；目标是保证 Rust 计算符合 Rust 实际内存布局和 owned heap 使用。Java 侧该估算主要服务于索引期间 RAM budget / flush 判断，Rust 侧也应优先统计对象长期持有的 retained heap / owned allocation；普通栈上结构体本体、局部变量和不持有堆内存的控制结构不应计入 flush budget。Rust 结构体本体大小应以 `std::mem::size_of::<T>()` / `size_of_val` 这类编译器布局信息为准，仅当该本体确实位于 retained heap 中（例如 `Box<T>` payload、`Vec<T>` 元素区）才纳入 RAM 估算，不额外套 Java 对象对齐；`Vec`、`String` 等动态内存按其 capacity 和元素/字节大小计算，但不试图包含标准库无法稳定暴露的 allocator 元数据或分配器内部 rounding。
  - Rust 内存统计要区分两个概念：结构体的 inline size，以及该结构体内部额外拥有的 owned heap。`size_of::<T>()` 返回 `T` 的完整 inline layout，包括普通字段、`String` / `Vec` 等控制值和 padding，但不会递归统计这些字段指向的动态 buffer。
  - `Accountable` 只需要保留 `ram_bytes_used()` 作为统一入口，不为相同目的再增加 `owned_heap_bytes_used()` 等第二套方法。Rust 中 `ram_bytes_used()` 的明确语义是：只统计当前结构体通过字段向下持有的堆内存，不包含 `size_of::<Self>()`，也不包含当前结构体本身可能位于栈上或其他 allocation 中的 inline storage。
  - 结构体自身无法知道它位于栈、`Vec` 元素区、`Box` payload，还是另一个结构体的 inline 字段中，因此 `ram_bytes_used()` 不应自行加入 `size_of::<Self>()`。真正创建堆存储的外层容器负责加入 payload / 元素的 inline size。
  - `ram_usage_estimator.rs` 使用模块级常量和函数，不定义空的 `RamUsageEstimator` 结构体包装静态方法；调用方直接导入并调用 `size_of_vec`、`size_of_string` 等函数。
  - `Vec<A>` 的 retained heap 计算为 `size_of_vec(&vec) + vec.iter().map(Accountable::ram_bytes_used).sum()`，也就是 `vec.capacity() * size_of::<A>()` 加上所有已初始化 `A` 的 `ram_bytes_used()`。第一部分统计整个已分配元素区，包括 `capacity - len` 个未初始化槽位；第二部分只遍历 `len` 个已初始化元素，因为未初始化槽位没有内部 owned allocation。
  - `Box<A>` 的 retained heap 计算为 boxed payload 的 `size_of_val` 加上 `A::ram_bytes_used()`。如果 `A` 是另一个结构体 `B` 的 inline 字段，则 `B::ram_bytes_used()` 直接调用 `A::ram_bytes_used()`，不加入 `size_of::<A>()`；当外层 `Vec<B>` 或 `Box<B>` 统计 `B` 的 inline storage 时，其中已经包含 `A` 的 inline storage。
  - 对 `Rc<A>` / `Arc<A>` 字段，如果当前 `Accountable` 对象被明确选为该共享 allocation 的统计根，则计算堆上的 `A` payload（`size_of_val(rc_or_arc.as_ref())`）并递归加入 `A` 持有的动态内存；不加入当前结构体内部的 `Rc` / `Arc` handle，因为它属于当前结构体的 inline storage。标准库没有稳定公开引用计数控制块布局，因此不估算 strong/weak 计数、allocator metadata 或分配器 rounding。
  - `Rc<Vec<T>>` / `Arc<Vec<T>>` 的典型统计为 `size_of_val(shared.as_ref()) + size_of_vec(shared.as_ref())`：第一项是共享 allocation 中 `Vec<T>` 控制值的 inline payload，第二项是 `Vec<T>` 单独分配的元素 buffer，按 `capacity * size_of::<T>()` 计算。若 `T` 还持有动态内存，还要遍历已初始化元素并调用其 `ram_bytes_used()`。例如 `IntArrayDocIdSet` 的 `Rc<Vec<i32>>` 只需前两项，因为 `i32` 没有内部 owned heap。
  - `Rc` / `Arc` clone 只增加共享 handle，不产生新的 payload 或元素 buffer。若多个 `Accountable` 根同时引用同一 allocation，汇总方必须选择唯一归属或去重；不能让每个根都把同一共享 allocation 完整累加。临时 iterator/view 仅共享主对象的数据时，通常不应作为另一份独立 retained resource 再次计入总量。
  - 多层嵌套仍遵循相同规则：每个结构体的 `ram_bytes_used()` 只递归汇总其字段产生的 owned heap；遇到 `Vec<T>` 时加入该 `Vec` 的直接元素区并调用每个已初始化元素的 `ram_bytes_used()`；遇到普通 inline 结构体字段时只调用该字段的 `ram_bytes_used()`。
  - 不应把上述规则描述成“移除栈字段”。字段是否计入取决于其存储是否已经被外层 allocation 的 inline size 覆盖，而不是字段在语义上属于“栈”还是“堆”。例如位于 `Vec<A>` 元素区中的 `A.id`、`A` 内部的 `String` 控制值和 `Vec` 控制值都实际位于该 `Vec` 的堆元素区，已由 `capacity * size_of::<A>()` 覆盖。
  - `String` 的额外 owned heap 按字节 capacity 统计；`Vec<T>` 的直接 buffer 按 `capacity * size_of::<T>()` 统计。若 `T` 本身还拥有动态内存，通用 `size_of_vec<T>` 只能统计 `Vec` 的直接元素区，调用方还必须遍历已初始化元素并统计其 owned heap。
  - 对 `Arc`、`Rc`、借用引用或其他共享/非拥有关系，必须明确统计所有权边界，避免同一 allocation 被多个对象重复计数；通用泛型函数不能仅凭 `T` 自动、准确地递归判断深层 owned heap。
- 验证方式：
  - 优先查找并迁移/对照 Java Lucene 的相关测试，用于理解预期行为和边界条件。
  - 转换 Java 代码到 Rust 时，不要自行定义额外的、Java 中不存在的单元测试；测试迁移应以 Java Lucene 已有测试为准，除非用户明确要求补充。
  - Java 中标记为 `@Nightly` 的测试迁移到 Rust 时，必须依次使用 `#[cfg(feature = "nightly")]`、`#[test]`、`#[ignore = "nightly"]` 三个属性；不能只添加 `#[ignore = "nightly"]`，避免 nightly 测试在未启用 `nightly` feature 时仍被编译进普通测试目标。
  - 源码和单元测试代码转换完成后，只需要验证 Rust 代码能够通过编译；默认不运行单元测试。
  - 如果排查后确认失败原因是单元测试本身存在错误，并对该单元测试进行了修复，则修复后必须运行对应的目标单元测试，验证修改确实解决了原失败；这种情况不受“默认不运行单元测试”的限制。应优先使用原失败时的随机 seed 复现和验证。
  - 修改 Rust 代码后默认不执行 `cargo tidy`；只有用户明确要求，或用户要求“提交代码”/“推送”而触发上述完整流程时才执行。
  - 如任务范围较小，优先运行目标 crate/module 的编译检查；只有用户明确要求时才运行测试。
  - 对边界条件补充或迁移测试代码，尤其是空输入、极值、溢出、排序稳定性、编码长度、随机化测试覆盖到的行为，但交付验证仍以编译通过为准。
- 注意事项：
  - 不为了“Rust 风格”改变 Lucene 的核心逻辑和可观察行为。
  - 不为了“整理代码”或“Rust 风格分组”重新排列 Java 中已有成员的声明顺序；方法、结构体和内部辅助类型的先后顺序也是迁移 fidelity 的一部分。
  - 注释内容要求语义一致，不允许为了润色而改变技术含义；但 Java 注释格式和 Java 专有语言术语必须转换成 Rust 风格和 Rust 术语。
  - 不允许自行发挥生成 Java 中不存在的额外方法、helper、抽象层或去重结构；除非用户明确要求或 Rust 编译/所有权模型确实无法直接表达，并且需要先说明原因。
  - 不做无关重构；保持迁移范围清晰，方便和 Java 源码逐行核对。
  - 代码说明中应尽量引用对应 Java 文件或类，方便后续继续迁移。

后续可以按下面格式追加：

### Java 可覆写类型在 Rust 中的 `Base` / `Defaults` / `Hook` 静态分发设计

- 适用场景：Java Lucene 通过继承、匿名子类或测试子类覆写生产类型的方法，而 Rust 对应类型仍需要保留静态分发、泛型信息和接近 Java 的 `super.method(...)` 语义时。当前已确认的参考实现是 `ConcurrentMergeScheduler` 与 `OneMerge`。
- 已确认设计：
  - Rust owner 结构体继续保存生产状态，同时持有一个 hook enum：`ConcurrentMergeScheduler` 持有 `ConcurrentMergeSchedulerHook`，`OneMerge` 持有 `OneMergeHook<D, CR>`。owner 对外方法通过 hook 分发，不使用 `dyn`、`Any` 或运行时向下转型。
  - `*Base` trait 表达 Java 的可覆写方法集合，`*Defaults` 集中保存 Java 原类的默认实现。Java 覆写中的 `super.method(...)` 必须直接调用对应的 `*Defaults::method(...)`，不能再次调用 owner 的分发入口而形成递归。
  - `ConcurrentMergeSchedulerBase` 的非覆写方法可以通过 trait 默认方法自动委托给 `ConcurrentMergeSchedulerDefaults`；`OneMergeBase` 当前声明完整覆写接口，具体 hook 对未覆写的方法显式委托给 `OneMergeDefaults`。新增代码应沿用各自现有结构，不为形式统一而重构。
  - hook enum 的 `Default` 变体代表未覆写的 Java 原类行为；生产环境确实存在的实现变体不加 `#[cfg(test)]`，只服务 Java 测试子类的变体必须使用 `#[cfg(test)]`。enum 对每个可覆写方法都要穷尽分发所有变体。
  - Java 匿名子类在 Rust 中可以转换成命名 hook struct，这是 Rust 无匿名继承所需的表达；该 struct 只实现 Java 实际覆写的方法，其余方法委托 `*Defaults`，不能顺便加入 Java 中不存在的行为、helper 或抽象。
  - trait 覆写面只纳入需要迁移且 Rust 确实支持其语义的 Java 可覆写方法。不能仅因为 Java 有某个方法就发明 Rust-only 能力；例如 Rust 不实现 Java intra-merge 并行 executor 语义时，不定义 `IntraMergeExecutor`，也不保留 `get_intra_merge_executor()` hook。
  - 必须先确认 Java 测试匿名类的直接父类。直接继承 `ConcurrentMergeScheduler` 的覆写应使用该测试专用 CMS hook，不能为了复用而伪装成 `SuppressingConcurrentMergeScheduler`；只有 Java 确实继承 `SuppressingConcurrentMergeScheduler` 时，才使用它及其 `ExpectedMergeException`/`isOK` 语义。例如 `TestIndexFileDeleter.testExcInDecRef` 的 `fake fail` 处理属于专用 CMS hook，不属于 `ExpectedMergeException`。
  - hook/owner 如果会被 Rust `Clone`，而 Java 语义是同一个子类实例共享状态，则 hook 中的计数器、latch、失败标志、集合和锁等状态必须通过 `Arc`、atomic 或共享锁保持同一份状态，不能让 clone 产生独立测试状态。
  - Java `try/finally`、`catch(Throwable)`、同步块、异常包装和 `onMergeFinished` 调用范围必须在 hook 方法内逐段保真；不能用额外 RAII guard 压平 Java 控制流。确实需要覆盖 Rust panic 时，使用 `catch_unwind` 明确表达 Java 的 `finally`/`Throwable` 范围。
- 测试代码归属：
  - 生产源码中的 hook enum 在 `#[cfg(test)]` 下需要引用的命名测试 hook，放入 `test_framework/core/index/` 下对应 Java 测试类的 snake_case 文件，并保留用于快速搜索的同名空结构体。
  - Java 测试类专用 hook 不放入无关的通用 helper；实际 `#[test]` 用例仍放在对应的 `src/test` 目录。
- 新增覆写的步骤：
  - 先定位 Java 的直接父类、实际 `@Override` 方法、`super` 调用、共享字段和异常控制流。
  - 将确实需要的覆写方法加入或复用现有 `*Base` 接口，在 `*Defaults` 中保留原类默认实现。
  - 为 Java 具体子类增加对应 hook struct 和 hook enum 变体，并补全该 enum 的所有分发 match。
  - 迁移 Java 原测试验证该覆写，不额外发明测试；至少编译对应测试目标。用户明确要求运行测试时，再运行目标测试和相关并发测试。

### `IndexReader` context 与 `MultiReader` 的 leaf/composite 静态设计

- 适用场景：修改 `IndexReader`、`LeafReader`、`CompositeReader`、`IndexReaderContext`、`MultiReader`，或继续把 Java 中接收 `IndexReader` 的 API 从 Rust 的 leaf/composite 专用签名恢复为统一接口时。
- 已确认设计：
  - `IndexReader::get_context()` 是统一入口；具体返回 `LeafReaderContext` 还是 `CompositeReaderContext` 由关联类型 `ContextKind` 静态映射。`LeafReader` 固定 `ContextKind = LeafReaderContextKind`，`CompositeReader` 固定 `ContextKind = CompositeReaderContextKind`。
  - 不再使用 `IndexReaderEnum`、`IndexReaderEnum2/3` 在 leaf/composite 之间做枚举分发。Java 参数是 `IndexReader` 时，Rust 优先写成 `IR: IndexReader`；只有 API 本身确实是 per-segment 或 composite tree 操作时才约束为 `LeafReader` / `CompositeReader`。
  - `MultiReader` 当前为 `MultiReader<R>`，只有 `new(Vec<R>)` 一个构造入口；直接复用 `R::ContextKind: MultiReaderKind<R>` 选择 leaf traversal，不再额外携带 `K`、`PhantomData<K>`、`LeafSubReaders` / `CompositeSubReaders` 或 `MultiLeafReader` / `MultiCompositeReader` 别名。
  - `MultiReader` 只允许 leaf-only 或 composite-only。`Vec<R>` 保证 sub-reader 的具体类型同构，`ContextKind` 保证该 `R` 只属于一种 reader 类别；不要重新引入 mixed reader enum。
- 注意事项：代码中泛型名 `CR` 既可能表示 `CompositeReader`，也常表示 `CodecReader`。审计接口分叉时必须看 trait bound，不能只按变量名判断；`CodecReaderEnum2`、`SortingCodecReaderEnum` 等都属于 leaf/codec 实现之间的同接口枚举，不是 leaf/composite 分叉。

```md
### Skill 名称

- 适用场景：
- 操作步骤：
- 验证方式：
- 注意事项：
```

## `rlucene` Jenkins CI 部署记忆

### 已部署拓扑

- Jenkins CI 已于 2026-07-25 完成并通过真实构建验证。Jenkins
  对用户可访问的地址是 `http://192.168.3.15:8080/`；Jenkins 所在
  Ubuntu 虚拟机内部地址是 `192.168.132.129`，SSH 用户是 `xugang`。
  Mac 不一定能直接路由到虚拟机内部地址，因此管理 Jenkins 时优先使用
  可访问的 Jenkins Web 地址和已登录的 Google Chrome 会话。
- 当前与本仓库 CI 相关的保留任务是：
  - `rlucene-ci`：Pipeline from SCM，脚本路径为 `Jenkinsfile`，只测试
    `Rustify-All/rlucene:main`；当前按用户要求保持禁用，后续重新安排任务
    时再决定是否启用。
  - `legency`：旧 Freestyle 测试任务，保持禁用，只用于保留五万多次历史
    构建记录，不再作为主 CI。
- Jenkins 每两分钟检查一次 `main`。同一 commit 仍会直接运行 nextest
  和 doctest，但会跳过依赖/基础设施预检并复用 Git、Cargo 和 target
  缓存；新 commit 才执行完整预检。
- Jenkins 使用仓库中的配置作为唯一来源。长期维护时优先修改：
  `rlucene/Jenkinsfile`、`rlucene/.config/nextest.toml` 和
  `rlucene/ci/jenkins/README.md`，不要只在 Jenkins 页面里临时改
  Pipeline 内容。
- Jenkins Git SSH 凭据 ID 为 `github-ssh`。只记录凭据 ID，任何 Secret
  或私钥实值都不得写入仓库、记忆、构建参数或日志。

### nextest、超时和诊断

- 常规 Rust 测试使用
  `cargo nextest run --profile ci --workspace`；nextest 不运行 doctest，
  因此另行执行 `cargo test --workspace --doc -q`。
- `.config/nextest.toml` 当前设置：60 秒标记 `SLOW`，
  `terminate-after = 6`，因此 60 秒只产生告警；测试运行到 300 秒时，
  Jenkins 会记录 nextest 当前运行测试、进程树、系统负载、线程 `/proc`
  状态、内核等待栈，以及可用时由 `eu-stack`、`gdb` 或 `pstack` 生成的
  用户态堆栈；单个测试约 360 秒才会被终止并报告 `TIMEOUT`。
  `fail-fast = false`，失败输出保留，成功输出不打印。
- 主 CI 对 nextest 使用 20 分钟整套测试外层超时，对 doctest 使用
  4 分钟超时，整个 CI Pipeline 使用 30 分钟超时。
- nextest 会在日志和 JUnit 中保留具体失败或超时测试的诊断信息，供人工
  排查。整套测试外层 124/137、Jenkins 被杀死、网络/磁盘/工具链错误应
  归类为基础设施失败。
- nextest JUnit 的真实来源路径固定为工作区相对路径
  `target/nextest/ci/junit.xml`，不跟随 `CARGO_TARGET_DIR`。主 CI 将它
  复制为 `nextest-junit.xml`。
- 主 CI 归档 `nextest.log`、`nextest-junit.xml`、
  `nextest-diagnostics.log`、`doctest.log`。

### 缓存、磁盘与已验证基线

- `rlucene-ci` 的持久化 Cargo target 是
  `/var/jenkins_home/cargo-target/rlucene-ci`。不要每两分钟运行
  `cargo clean`，否则会丢失编译缓存。
- 每次 CI 构建必须在日志中打印 Jenkins home、`/tmp`、Cargo target
  的构建前后磁盘状态。需要清理时先确认没有构建正在使用，仅清理明确
  的旧 target；旧的约 99G `/var/jenkins_home/cargo-target/rlucene`
  已经在用户确认后删除。
- 最终验收基线：PR `Rustify-All/rlucene#133` 引入 nextest，PR `#134`
  修正 JUnit 工作区路径；合并提交 `9793ce6e77baf81058fa7ea235f95c28680c1c2c`
  在 Jenkins `rlucene-ci` 构建 `#49` 成功。
- 构建 `#49` 归档了 `doctest.log`、`nextest.log` 和
  `nextest-junit.xml`。JUnit 文件大小为 985851 字节，包含 4807 个
  `<testcase>`，没有 `<failure>` 或 `<error>`。这证明 nextest、
  doctest、JUnit 诊断和归档链路已经真实生效。
