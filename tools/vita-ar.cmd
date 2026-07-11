@echo off
setlocal
set "VITA_TOOL=arm-vita-eabi-ar"
call "%~dp0vita-tool.cmd" %*
exit /b %ERRORLEVEL%
