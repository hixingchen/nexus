# Nexus Linux 服务端版（无界面）

把项目/服务配置集中管理，并在 Linux 开机时按顺序拉起、做就绪检查、崩溃重启。
**没有 GUI**：管理入口是 `nexusctl` 命令，常驻进程是 `nexus-boot-runner`，由 systemd 托管。

## 环境要求

- Linux x86_64（在 Ubuntu 22.04 上构建，glibc ≥ 2.35）
- systemd（user 或 system 模式）
- 不需要 WebKitGTK、Node、数据库

## 安装

```bash
tar xzf nexus-<版本>-linux-x86_64.tar.gz
cd nexus-<版本>-linux-x86_64

# 用户级（默认）：装到 ~/.local/bin，配置 ~/.config/nexus/nexus.toml
./install.sh

# 系统级：装到 /usr/local/bin，配置 /etc/nexus/nexus.toml（需要 root）
sudo ./install.sh --system
```

## 快速开始（用户级）

```bash
nexusctl project add /srv/app --name app
nexusctl service add -p app -n api -c "npm run start" \
  --workdir /srv/app/api --env NODE_ENV=production \
  --boot --order 10 --ready tcp:127.0.0.1:3000 --required
nexusctl service list

# 生成 manifest + 安装/启动 systemd unit
nexusctl boot install
nexusctl boot status
```

要在**无人登录**时也启动，开启 linger（通常需要管理员授权）：

```bash
sudo loginctl enable-linger "$USER"
```

服务器级安装：

```bash
sudo nexusctl --system --run-as deploy boot install
```

## 常用命令

```bash
# 项目
nexusctl project list [--json]
nexusctl project add <路径> [--name <名字>]
nexusctl project remove <名字>

# 服务
nexusctl service list [--json]
nexusctl service add -p <项目> -n <服务> -c <命令> \
  [--workdir <目录>] [--env K=V]... [--boot] [--order N] \
  [--ready none|wait:5|tcp:host:port|log:关键字|JSON] [--required] [--no-crash-restart]
nexusctl service set <项目> <服务> [--command ...] [--workdir ...] [--env K=V]... \
  [--clear-env] [--order N] [--ready ...] [--boot|--no-boot] [--required|--no-required] \
  [--crash-restart|--no-crash-restart]
nexusctl service remove <项目> <服务>
nexusctl service start|stop|restart <项目> <服务>
nexusctl service enable-boot|disable-boot <项目> <服务>

# 开机启动 / systemd
nexusctl boot install
nexusctl boot uninstall
nexusctl boot unit           # 只打印 unit（检查/调试，不安装）
nexusctl boot shutdown       # 停 runner 与其管理的全部服务（unit 保留）
nexusctl boot sync           # 配置改了以后重新生成 manifest 并 reload
nexusctl boot status [--json]
nexusctl boot logs <项目> <服务> [--lines 200]
```

全局参数：`--config <路径>`、`--data-dir <路径>`、`--system`、`--run-as <用户>`、`--runner <路径>`。
环境变量 `NEXUS_CONFIG` / `NEXUS_DATA_DIR` 与参数等价（便于测试/多实例）。

## 配置

配置是唯一真相：`~/.config/nexus/nexus.toml`（用户级）或 `/etc/nexus/nexus.toml`（系统级）。
示例见 `share/nexus.example.toml`。

- `[[projects]]`：`name`、`path`
- `[[services]]`：
  - `project`、`name`（只允许字母/数字/`-`/`_`，用于日志文件名）
  - `command`、`workdir`（省略 = 项目路径）
  - `env`（服务级，覆盖全局 `[env]`）
  - `boot`（开机自动拉起）、`order`（小的先启动）
  - `ready`：`none` / `wait:5` / `tcp:127.0.0.1:3000` / `log:关键字` / JSON
  - `required`（未就绪阻断后续）、`crash_restart`
- 非开机服务也可以写在配置里：runner 会管理它，只是不会自动拉起，可用 `service start`。

**环境快照**：生成 manifest 时抓取 `PATH`、`HOME`、`JAVA_HOME`、`NVM_DIR` 等路径类变量
（不含密钥/代理凭据）。安装 Node 等工具后若 PATH 变了，执行 `nexusctl boot sync` 刷新。

**注意**：用 `nexusctl` 修改配置会重写 TOML，手写注释可能丢失；直接编辑文件则完全支持。

## 运行与排错

- 状态：`nexusctl boot status`
- runner 日志：`~/.nexus/boot/systemd/runner.log`（系统级 `/var/lib/nexus/boot/systemd/runner.log`）
- 服务日志：同目录 `logs/<project>_<service>.log`
- systemd：

  ```bash
  systemctl --user status nexus-boot-runner.service
  journalctl --user -u nexus-boot-runner.service -e
  ```

- `boot status` 显示 linger off：`sudo loginctl enable-linger "$USER"`。
- 启动失败：看 runner.log 与服务日志；命令在服务的工作目录下用 `sh -c` 执行。
- 升级：覆盖两个二进制后执行 `nexusctl boot install`（重写 unit；若 unit 已在运行会 restart 以加载新版本）。
  restart 会停止当前所有服务（含非开机服务），开机服务随后按 manifest 重新拉起。

## 卸载

```bash
./uninstall.sh            # 用户级
sudo ./uninstall.sh --system
```

默认保留配置与日志；卸载前可先 `nexusctl boot uninstall`。
