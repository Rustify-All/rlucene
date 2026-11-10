# 复现 RLucene 基准测试

目录与[网站首页卡片](https://benchmark.amazingkoala.com.cn/)对应：

- `search-benchmark-game/`：Wikipedia 962 条查询、五种模式。
- `vector-search/`：SIFT1M、GloVe/Wiki 100/300维、MiniLM/Wiki384维、MPNet/Wiki768维的完整构图、Top100查询与双向索引验证。
- [`indexing/`](indexing/README.md)：索引写入基准，首个可选数据集是 Rally HTTP Logs 百万条。

Rally 写入数据集可直接下载[官方原始分片](https://rally-tracks.elastic.co/http_logs/documents-181998.json.bz2)，也可按[数据准备说明](datasets/rally-http-logs/README.md)自动下载并核对 SHA-256。

官方运行环境为 **macOS + Rust 1.98.1 + JDK 23 + Python 3.9+**。Python 只用标准库。
模型向量生成另需固定的 Python3.12 虚拟环境和模型依赖，详见
[多数据集复现指南](vector-search/REPRODUCING.md)；运行已有向量的基准不需要模型依赖。
其他平台可用 Codex 等 agent 调整平台调用，保留输入、调用流程、预热/采样和测量边界。
JDK 路径使用 `JAVA_HOME` 或 `--java-home`，索引和输出路径通过参数指定。

在仓库根目录执行：

```sh
python3 -B benchmark/run.py list
python3 -B benchmark/run.py indexing datasets
python3 -B benchmark/run.py vector-search presets
python3 -B benchmark/run.py vector-search plan --preset mpnet-wiki-768
python3 -B benchmark/run.py search-benchmark-game build
python3 -B benchmark/run.py search-benchmark-game smoke --index /path/to/wiki-index
python3 -B benchmark/run.py search-benchmark-game run --index /path/to/wiki-index

python3 -B benchmark/run.py vector-search build
python3 -B benchmark/run.py vector-search smoke --index /path/to/sift-index --data /path/to/sift-data
python3 -B benchmark/run.py vector-search run --index /path/to/sift-index --data /path/to/sift-data
```

不带 `--preset` 的向量命令保留原 SIFT 查询协议。新增预设入口按 `prepare-data`、
`truth`、`build-index`、`verify-index`、`query`、`report` 分阶段执行，`all` 串行执行完整
构建/兼容/查询；`build` 仅编译驱动。只有显式选择相应命令才下载、生成数据或构建索引，
查询可以复用已有索引，不会隐式重建。性能锁覆盖各重操作和清理/导出，所有失败与部分
结果保留。Cohere 暂未加入。配置、来源校验、codec冻结补丁及完整命令见上述指南。

首次 `build` 按固定清单下载并校验官方 JAR，同时按锁文件下载 Cargo 依赖。也可用
`--jars-dir` 指向已有的相同 JAR。Rust 驱动是独立 consumer，以真实 `path` 依赖当前
仓库、普通 release 构建；不会构建库内单元测试。源码和二进制各自记录身份，不能把
当前新构建直接当成历史网页中已测的那个程序。

数据准备命令见各套件 README。大语料、索引、JAR、target、运行结果留在忽略目录或
外部指定位置；Git 保存测试代码、完整查询集、参数、锁文件、上游版本和校验清单。
已有索引只读复用。全文与向量套件用各自的显式建索引命令，写入套件的 `smoke` 和 `run` 每次创建新索引。重建索引会记录新的文件身份，
全文套件可用 `--require-published-index` 强制要求原发布索引；没有伪造原索引下载地址。

默认五模式先做每端 4,810 次完整有序输出校验，再做每端/模式/批次至少 60 秒实际混合
请求预热、两批各 12 轮均衡 AB/BA。向量套件先核对每端全部 10,000 个查询的有序
ID/f32 分数位、独立 visited 路径和 Recall，再进行两批完整查询扫轮。新Wiki预设保留
全部2641条查询，评分位、排序、集合和visited差异逐项报告；SIFT及所有索引结构互读
保持严格校验。向量 CPU/RSS
为结果指标；不会用 CPU 负载作为门禁、删样或自动重测依据。

结果位于 `benchmark/runs/套件/时间戳/`，或 `--output` 指定的新目录。保留整个结果目录，
并保存 `.cache/套件/BUILD.json` 及构建日志。`RUN.json` 记录状态、源码/驱动/依赖/
机器/数据身份；`RESULT.json` 和 `REPORT.md` 为汇总，`per-query.csv` 与
`REGRESSIONS.json` 保留逐查询结果和所有回退。原始样本与完整正确性输出保留在结果目录。

Ctrl+C 会停止当前进程并保存已写数据，标记为停止，不会自动续跑或当成完整结果。
以下命令只重新汇总已有数据：

```sh
python3 -B benchmark/run.py search-benchmark-game summarize --output /path/to/run
python3 -B benchmark/run.py vector-search summarize --output /path/to/run
```

后续每种新测试方法增加对应卡片目录、上游固定版本、输入/生成方法及 SHA-256、两端驱动、
配置、正确性和测量边界，再登记统一入口。网页数据通过独立的导入流程发布；跑测试不会
自动修改或发布网站。历史性能、生成码、临时诊断和维护机器的私有路径不混入测试协议。
