#!/bin/bash
set -euo pipefail
package=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
"$package/featherpostctl" set-default-frontend --id dev.featherpost.ui
printf '\n已切回原版。关闭 Studio，再从菜单栏打开邮箱即可。\n'
