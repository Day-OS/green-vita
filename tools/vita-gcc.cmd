@echo off
setlocal
set "VITA_TOOL=arm-vita-eabi-gcc"
call "%~dp0vita-tool.cmd" %*
exit /b %ERRORLEVEL%
