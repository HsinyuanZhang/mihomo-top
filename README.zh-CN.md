# mihomo-top

`mihomo-top` 是一个实验性的独立 Linux 终端监视器，用于查看 [mihomo](https://github.com/MetaCubeX/mihomo) 控制器提供的连接流。它展示当前经过 mihomo 的流量，并可选地、尽力关联到本机进程和 tmux 窗格。本项目与 mihomo 没有隶属、赞助或背书关系。

程序以普通用户身份运行，不需要 root、守护进程、抓包或内核模块。

> **实验性 v0.1。** 输出适合实时排查，不是记账记录，也不保证涵盖主机全部流量。依赖输出前请阅读[限制说明](docs/limitations.md)。

[English README](README.md) · [输出 Schema](docs/output-schema.md) · [验证](docs/validation.md) · [测试](TESTING.md) · [路线图](docs/roadmap.md) · [贡献指南](CONTRIBUTING.md)

## 监视范围

程序读取 mihomo 控制器的连接数据，只覆盖该控制器当前已知的连接。因此，直连流量和其他代理产生的流量不在可见范围内。对可见连接，程序可显示目标、网络类型、规则、出站链路、字节计数和连接时长；能够可靠关联时，还会显示本机进程及 tmux 窗格。

TCP 归属采用保守的尽力策略：只会解析本机可见且所有者唯一的 socket。可见数据共享或无法唯一确定时显示 `UNKNOWN`。不可读或无法检测的共享所有者会限制工具能够确认的结论。v0.1 可以展示 UDP 连接事实，但不支持 UDP 的本机进程归属。

## 从源码安装

首个版本尚未发布到 crate 仓库，也没有预编译下载。需要 Rust 1.88 或更高版本，然后构建：

```sh
git clone https://github.com/HsinyuanZhang/mihomo-top.git
cd mihomo-top
cargo build --release --locked
install -Dm755 target/release/mihomo-top "$HOME/.local/bin/mihomo-top"
```

确认 `$HOME/.local/bin` 已在你的 `PATH` 中，然后验证安装：

```sh
mihomo-top --help
```

二进制程序只连接数值形式的回环控制器：`127.0.0.1:PORT` 或 `[::1]:PORT`。v0.1 有意不支持远程地址、域名、TLS 或其他控制器地址。

## 快速开始

通过受保护的本地文件提供控制器密钥，然后启动 TUI：

```sh
install -d -m 700 "$HOME/.config/mihomo-top"
install -m 600 /dev/null "$HOME/.config/mihomo-top/secret"
# 使用你常用的编辑器把控制器密钥写入该文件。
mihomo-top --controller 127.0.0.1:9090 --secret-file "$HOME/.config/mihomo-top/secret"
```

`--secret-file` 必须是普通文件、不是符号链接，且权限必须正好为 `0600`。也可通过环境变量 `MIHOMO_SECRET` 提供。密钥只用于控制器认证；JSON 输出仅含布尔值 `secret_in_use`，不会输出密钥。

直接运行 `mihomo-top` 时，只有标准输入和标准输出均为终端才会打开 TUI；任一不是终端时，同一命令会自动输出 JSONL：

```sh
mihomo-top --controller 127.0.0.1:9090 --secret-file "$HOME/.config/mihomo-top/secret" > frames.jsonl
```

用 `Ctrl-C` 或 `SIGTERM` 停止。网络工作线程会以较短间隔（当前为 250 ms）检查取消；tmux 查询有两秒截止时间，但这不是端到端关闭时间的硬性保证。监视器以只读方式访问 mihomo、`/proc` 和 tmux，不会修改它们；预期输出为标准输出上的 JSON/JSONL，诊断信息写到标准错误。若信号在管道阻塞时停止流，最后一行 JSONL 可能不完整。消费者必须忽略未以换行结束的最后一行；不保证投递或持久化。

## 命令与输出

| 调用方式 | 结果 |
| --- | --- |
| `mihomo-top` | 标准输入和标准输出均为终端时打开 TUI；否则输出 JSONL |
| `mihomo-top tui` | 强制打开 TUI |
| `mihomo-top --once` | 获取一帧并输出一个 JSON 文档 |
| `mihomo-top --stream` | 连续输出 JSONL，每行一个 JSON 文档 |
| `mihomo-top --format json` | 为单帧模式选择 JSON |
| `mihomo-top --format jsonl` | 为流模式选择 JSONL |
| `mihomo-top --version` / `-V` | 输出版本 |
| `mihomo-top --help` / `-h` | 输出帮助 |

`--format json` 仅用于单帧输出，`--format jsonl` 仅用于流输出。格式必须和模式一致；不兼容的组合会被拒绝。

```sh
# 输出一次机器可读的快照。
mihomo-top --once --controller 127.0.0.1:9090 --secret-file "$HOME/.config/mihomo-top/secret"

# 显式连续 JSONL。
mihomo-top --stream --format jsonl --controller [::1]:9090 --no-tmux

# TUI，不进行进程或窗格归属查询。
mihomo-top tui --no-attribution
```

完整字段和状态请参阅[输出 Schema](docs/output-schema.md)。

## 作为用户服务运行

仓库提供了 [examples/mihomo-top.service](examples/mihomo-top.service) 这个 systemd 用户单元示例。将二进制安装到 `~/.local/bin/mihomo-top` 后复制、重载并启动：

```sh
install -d "$HOME/.config/systemd/user"
cp examples/mihomo-top.service "$HOME/.config/systemd/user/mihomo-top.service"
systemctl --user daemon-reload
systemctl --user enable --now mihomo-top.service
journalctl --user -u mihomo-top.service -f -o cat
```

示例单元会启动 `--stream --no-tmux`，并且刻意不包含控制器密钥。若控制器需要认证，请先在受保护的位置放置密钥文件，再向其 `ExecStart` 添加 `--secret-file PATH`。如需窗格归属，请移除 `--no-tmux`，并添加指向适用本机 tmux socket 的 `--socket PATH`。

## 选项

| 选项 | 含义 |
| --- | --- |
| `--controller HOST:PORT` | 仅数值回环端点：`127.0.0.1:PORT` 或 `[::1]:PORT`。 |
| `--secret-file PATH` | 密钥文件；必须是非链接普通文件，权限正好为 `0600`。 |
| `MIHOMO_SECRET` | 未给出 `--secret-file` 时使用的环境变量后备值。 |
| `--socket PATH` | tmux socket 路径；默认使用当前/默认 tmux socket。 |
| `--no-tmux` | 禁用 tmux 窗格归属。 |
| `--no-attribution` | 禁用所有本机进程和窗格归属。 |
| `--tick-ms N` | TUI/流输出刷新间隔；默认 `1000`，范围 `100`–`60000`。 |
| `--timeout-secs N` | `--once` 超时秒数；默认 `10`。 |

同时提供密钥文件和 `MIHOMO_SECRET` 时，优先使用显式文件。请勿把密钥放在命令行中，因为本机其他进程可能看到它。

## 归属与隐私

本机归属是可选功能，只在本机 Linux 主机上执行。`--no-attribution` 会完全关闭；`--no-tmux` 保留进程查询但关闭 tmux 查询。程序不需要 root，因此其他用户拥有的 socket 可能无法归属。连接流及其目标元数据来自本机 mihomo 控制器；本工具不会建立连接历史，也不保证完整覆盖。

更多边界和失败场景见[docs/limitations.md](docs/limitations.md)。

## 测试

离线检查（`cargo test --locked`）不需要 mihomo。实机实验环境的运行方式见 [TESTING.md](TESTING.md)：脚本会下载固定版本的 mihomo，只使用本机 mock 代理，并核对每个进程的可见状态、控制器报告的目标和连通性。

```sh
scripts/test-env.sh
```
