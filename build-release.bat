@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"

rem ============================================================
rem  SakuGame 一键打包脚本（Tauri v2 + Vue 3 + Rust）
rem
rem  默认：构建绿色版 exe -> release\SakuGame.exe（前端已内嵌，双击即运行）
rem  参数：
rem    --run       构建完成后立即启动 exe
rem    --bundle    额外生成安装包（msi + nsis，首次需联网下载打包工具，较慢）
rem    --help      显示本帮助
rem ============================================================

set "APP_NAME=SakuGame"
set "RELEASE_DIR=release"
set "DO_BUNDLE=0"
set "DO_RUN=0"

rem ---------- 参数解析 ----------
:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--help" goto usage
if /i "%~1"=="-h" goto usage
if /i "%~1"=="--bundle" (
    set "DO_BUNDLE=1"
    shift
    goto parse_args
)
if /i "%~1"=="-b" (
    set "DO_BUNDLE=1"
    shift
    goto parse_args
)
if /i "%~1"=="--run" (
    set "DO_RUN=1"
    shift
    goto parse_args
)
if /i "%~1"=="-r" (
    set "DO_RUN=1"
    shift
    goto parse_args
)
echo [警告] 忽略未知参数: %~1
shift
goto parse_args

:args_done

rem ---------- [1/4] 环境检查 ----------
echo ============================================================
echo    SakuGame 一键打包（Tauri v2）
echo ============================================================
echo.
echo [1/4] 检查构建环境...

where node >nul 2>nul
if errorlevel 1 (
    echo    [错误] 未找到 node，请先安装 Node.js 并加入 PATH
    goto fail
)

where pnpm >nul 2>nul
if errorlevel 1 (
    echo    [错误] 未找到 pnpm，请先执行: npm i -g pnpm
    goto fail
)

where cargo >nul 2>nul
if errorlevel 1 (
    echo    [错误] 未找到 cargo，请先安装 Rust 工具链: https://rustup.rs
    goto fail
)

if not exist "src-tauri\tauri.conf.json" (
    echo    [错误] 当前目录不是 SakuGame 源码根目录（缺少 src-tauri\tauri.conf.json）
    goto fail
)

rem 读取版本号（package.json 为唯一来源）
for /f "delims=" %%v in ('node -p "JSON.parse(require('fs').readFileSync('package.json','utf8')).version"') do set "APP_VERSION=%%v"
if not defined APP_VERSION set "APP_VERSION=unknown"
echo    版本: v%APP_VERSION%

rem ---------- [2/4] 依赖 ----------
if not exist "node_modules\.bin\tauri.cmd" (
    echo.
    echo [2/4] 安装前端依赖（pnpm install）...
    call pnpm install --frozen-lockfile
    if errorlevel 1 (
        echo    [错误] 依赖安装失败
        goto fail
    )
) else (
    echo.
    echo [2/4] 前端依赖已就绪，跳过安装
)

rem ---------- [3/4] 构建 ----------
echo.
if "%DO_BUNDLE%"=="1" (
    echo [3/4] 构建中：前端 + Rust release + 安装包
) else (
    echo [3/4] 构建中：前端 + Rust release
)
echo    首次构建需编译全部依赖（含 SQLite），约 10-20 分钟；之后走增量缓存会快很多。
echo    前端构建由 tauri 自动执行（pnpm build，含 vue-tsc 类型检查）。
echo.

if "%DO_BUNDLE%"=="1" (
    call "node_modules\.bin\tauri.cmd" build
) else (
    call "node_modules\.bin\tauri.cmd" build --no-bundle
)
if errorlevel 1 (
    echo.
    echo    [错误] 构建失败，请查看上方 Rust / 前端报错信息
    goto fail
)

rem ---------- [4/4] 收集产物 ----------
set "EXE_SRC="
if exist "src-tauri\target\release\sakugame.exe" set "EXE_SRC=src-tauri\target\release\sakugame.exe"
if not defined EXE_SRC if exist "src-tauri\target\release\SakuGame.exe" set "EXE_SRC=src-tauri\target\release\SakuGame.exe"
if not defined EXE_SRC (
    echo.
    echo    [错误] 未找到构建产物：src-tauri\target\release\*.exe
    goto fail
)

if not exist "%RELEASE_DIR%" mkdir "%RELEASE_DIR%"
copy /y "%EXE_SRC%" "%RELEASE_DIR%\%APP_NAME%.exe" >nul
if errorlevel 1 (
    echo    [错误] 拷贝产物到 %RELEASE_DIR%\ 失败
    goto fail
)

set "EXE_SIZE=0"
for %%A in ("%RELEASE_DIR%\%APP_NAME%.exe") do set "EXE_SIZE=%%~zA"
set /a EXE_MB=%EXE_SIZE%/1048576

rem WebView2 运行时检测（仅提示，不阻断）
set "WV2="
reg query "HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" /v pv >nul 2>nul
if not errorlevel 1 set "WV2=1"
if not defined WV2 (
    reg query "HKCU\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" /v pv >nul 2>nul
    if not errorlevel 1 set "WV2=1"
)

echo.
echo [4/4] 完成！
echo ============================================================
echo    绿色版 exe : %cd%\%RELEASE_DIR%\%APP_NAME%.exe  ^(%EXE_MB% MB^)
if "%DO_BUNDLE%"=="1" (
    echo    安装包目录 : %cd%\src-tauri\target\release\bundle\
    echo                 ^(nsis\*.exe 与 msi\*.msi^)
)
echo    数据目录   : %%APPDATA%%\com.saku.sakugame\  ^(sakugame.db^)
echo ============================================================
if not defined WV2 (
    echo.
    echo    [提示] 未检测到 WebView2 运行时。若 exe 启动后白屏/闪退，
    echo           请安装 WebView2 Runtime: https://developer.microsoft.com/microsoft-edge/webview2/
)
echo.
echo    说明：exe 内已内嵌前端资源，可单独拷贝到任意目录运行；
echo          数据库与设置保存在用户数据目录，不随 exe 位置变化。

if "%DO_RUN%"=="1" (
    echo.
    echo    正在启动 %RELEASE_DIR%\%APP_NAME%.exe ...
    start "" "%RELEASE_DIR%\%APP_NAME%.exe"
)

echo.
pause
endlocal
exit /b 0

:usage
echo 用法: build-release.bat [选项]
echo.
echo   （无参数）   构建绿色版 exe -^> release\SakuGame.exe
echo   --run,   -r  构建完成后立即启动 exe
echo   --bundle,-b  额外生成安装包 msi + nsis
echo   --help,  -h  显示本帮助
exit /b 0

:fail
echo.
echo [失败] 打包未完成，请根据上方报错排查。
echo.
pause
endlocal
exit /b 1
