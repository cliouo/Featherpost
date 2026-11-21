#!/bin/bash
set -euo pipefail
package=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
fail() { printf '\n安装未完成。请先打开 Featherpost，让本机后台运行，再试一次。\n'; read -r -p '按回车关闭…'; }
trap fail ERR
"$package/featherpostctl" status >/dev/null
"$package/featherpostctl" plugin install --frontend --source "$package/frontend"
"$package/featherpostctl" set-default-frontend --id dev.featherpost.studio
# Open the managed copy, not the download, so future launches use the same instance.
open "$HOME/Library/Application Support/Featherpost/frontends/dev.featherpost.studio/Featherpost Studio.app"
printf '\nStudio 已安装并设为默认前端，原版仍然保留。\n'
