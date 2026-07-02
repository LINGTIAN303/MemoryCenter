@echo off
REM Hippocampus C 集成测试构建脚本（Windows MSVC）
REM
REM 用法：
REM   build.bat build   REM 构建 Rust 动态库 + 编译 C 测试
REM   build.bat test    REM 运行 C 测试
REM   build.bat all     REM build + test
REM   build.bat clean   REM 清理

setlocal

set PROJECT_ROOT=..\..
set FFI_INCLUDE=%PROJECT_ROOT%\crates\hippocampus-ffi\include
set LIB_DIR=%PROJECT_ROOT%\target\release
set LIB_NAME=hippocampus.dll

if "%1"=="" goto all
if "%1"=="build" goto build
if "%1"=="test" goto test
if "%1"=="all" goto all
if "%1"=="clean" goto clean
echo Unknown target: %1
exit /b 1

:all
call :rust_lib
call :c_test
call :run_test
goto end

:build
call :rust_lib
call :c_test
goto end

:test
call :run_test
goto end

:rust_lib
echo [1/3] 构建 Rust 动态库...
pushd %PROJECT_ROOT%
cargo build --release -p hippocampus-ffi
if errorlevel 1 (
    echo 构建失败
    popd
    exit /b 1
)
popd
goto :eof

:c_test
echo [2/3] 编译 C 测试...
cl test.c /I %FFI_INCLUDE% /link %LIB_DIR%\%LIB_NAME%
if errorlevel 1 (
    echo 编译失败（请确保已安装 VS Build Tools 并在开发者命令提示符中运行）
    exit /b 1
)
goto :eof

:run_test
echo [3/3] 运行 C 测试...
set PATH=%LIB_DIR%;%PATH%
test.exe
goto :eof

:clean
if exist test.exe del test.exe
for /d %%D in (tmp_test_*) do rmdir /s /q %%D
goto end

:end
endlocal
