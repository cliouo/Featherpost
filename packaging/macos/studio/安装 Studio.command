#!/bin/bash
set -euo pipefail
package=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
fail() { printf '\n安装未完成。请先打开 Featherpost，让本机后台运行，再试一次。\n'; read -r -p '按回车关闭…'; }
trap fail ERR
"$package/featherpostctl" status >/dev/null
# Ask only Studio to terminate through its normal save/close guards. Never kill it
# or remove the installed frontend to work around version or unsaved-draft checks.
osascript -l JavaScript <<'JXA'
ObjC.import('AppKit');
const bundleId = 'dev.featherpost.studio';
const running = $.NSRunningApplication.runningApplicationsWithBundleIdentifier(bundleId);
for (let i = 0; i < running.count; i++) running.objectAtIndex(i).terminate;
const deadline = Date.now() + 30000;
while ($.NSRunningApplication.runningApplicationsWithBundleIdentifier(bundleId).count > 0) {
  if (Date.now() >= deadline) throw Error('Studio 仍在运行，升级已停止。请先处理窗口内未保存的草稿，再重试。');
  $.NSRunLoop.currentRunLoop.runUntilDate($.NSDate.dateWithTimeIntervalSinceNow(0.1));
}
JXA
"$package/featherpostctl" plugin install --frontend --source "$package/frontend"
"$package/featherpostctl" set-default-frontend --id dev.featherpost.studio
# Open the managed copy, not the download, so future launches use the same instance.
open "$HOME/Library/Application Support/Featherpost/frontends/dev.featherpost.studio/Featherpost Studio.app"
printf '\nStudio 已安装并设为默认前端，原版仍然保留。\n'
