@echo off
setlocal

if "%VITASDK%"=="" (
  echo VITASDK is not set. 1>&2
  exit /b 1
)

set "PKG_CONFIG_EXE=pkg-config.exe"
where "%PKG_CONFIG_EXE%" >nul 2>nul
if errorlevel 1 (
  if exist "C:\msys64\mingw64\bin\pkg-config.exe" (
    set "PKG_CONFIG_EXE=C:\msys64\mingw64\bin\pkg-config.exe"
  )
)

set "PKG_CONFIG_DIR="
set "PKG_CONFIG_PATH="
set "PKG_CONFIG_SYSROOT_DIR="
set "PKG_CONFIG_LIBDIR=%VITASDK%\arm-vita-eabi\lib\pkgconfig;%VITASDK%\arm-vita-eabi\share\pkgconfig"

"%PKG_CONFIG_EXE%" --define-variable=VITASDK=%VITASDK% --define-prefix --static %*
