@echo off
setlocal

if "%VITA_TOOL%"=="" (
  echo Vita tool name is not set. 1>&2
  exit /b 1
)

if "%VITASDK%"=="" (
  echo VITASDK is not set. 1>&2
  exit /b 1
)

"%VITASDK%\bin\%VITA_TOOL%.exe" %*
