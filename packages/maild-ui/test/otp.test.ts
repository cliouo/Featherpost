import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import sharedVectors from "../../../crates/maild/tests/fixtures/otp_vectors.json";
import { detectOtpCode } from "../src/mailbox/otp";
import { OtpBar } from "../src/mailbox/ReaderParts";

describe("detectOtpCode：命中", () => {
  it.each([
    ["登录提醒", "您的验证码是 849201，5 分钟内有效，请勿泄露。", "849201"],
    ["【某银行】", "验证码：4821。如非本人操作请忽略。", "4821"],
    ["动态密码", "本次交易动态密码 30517246，有效期 10 分钟。", "30517246"],
    ["Your verification code", "Use 849201 to finish signing in.", "849201"],
    ["Sign in", "Your one-time passcode is 552093.", "552093"],
    ["Security alert", "Enter the code 7316 on the device.", "7316"],
    ["OTP for login", "123456 is your OTP. Do not share it.", "123456"],
    ["验证码", "您好：\n\n993021\n\n请在页面中输入以上数字完成验证。\n此致\n" + "x".repeat(200), "993021"],
  ])("%s / %s → %s", (subject, body, code) => {
    expect(detectOtpCode(subject, body)).toBe(code);
  });

  it("同时有日期与验证码时取验证码", () => {
    expect(detectOtpCode("通知", "您的订单 20261117 已发货，取件验证码 593021。")).toBe("593021");
    expect(detectOtpCode("Code", "Sent on 2026-11-17 12:30. Your verification code: 774512")).toBe("774512");
    expect(detectOtpCode("", "Your verification code on Nov 18, 2026 is 482910")).toBe("482910");
  });
});

describe("detectOtpCode：防误报", () => {
  it.each([
    ["没有语境词", "会议改到 3021 室，下午 1430 开始。"],
    ["日期", "验证码相关说明于 20261117 生效。"],
    ["带分隔日期", "验证码功能 2026-11-17 上线。"],
    ["长单号", "订单号 202611170001 的验证码说明见附件。"],
    ["手机号", "验证码已发送至 13800138000，请查收短信。"],
    ["分组手机号", "验证码已发送至 +86 138 0013 8000。"],
    ["横线手机号", "验证码已发送至 138-0013-8000。"],
    ["金额", "验证码服务费 ¥1280.00 已扣除。"],
    ["年份", "验证码服务自 2026 年起停止。"],
    ["英文日期缩写", "Please verify your login on Nov 18, 2026."],
    ["英文日期全称", "Verification link valid until 18 November 2026"],
    ["版权符号年份", "© 2026 Example Inc. Verify your email"],
    ["版权英文年份", "Verify your email\nCopyright 2026 Acme"],
    ["邮编", "Please confirm your zip code 94107 before checkout."],
    ["链接里的数字", "Verify your email: https://example.com/verify?id=88442211&t=1"],
    ["时间", "Your verification window is 10:30-11:45."],
  ])("%s", (_name, body) => {
    expect(detectOtpCode("通知", body)).toBeNull();
  });

  it("空输入安全", () => {
    expect(detectOtpCode("", "")).toBeNull();
    expect(detectOtpCode(undefined as never, null as never)).toBeNull();
  });
});

describe("detectOtpCode：跨语言共享向量（与 crates/maild/src/otp.rs 一致）", () => {
  it.each(
    sharedVectors.map((v) => [v.name, v.subject, v.body, v.expected] as const),
  )("%s", (_name, subject, body, expected) => {
    expect(detectOtpCode(subject, body)).toBe(expected);
  });
});

it("验证码条渲染代码与复制按钮", () => {
  const html = renderToStaticMarkup(createElement(OtpBar, { code: "849201", notify: () => {} }));
  expect(html).toContain("849201");
  expect(html).toContain("复制验证码");
  expect(html).toContain('aria-label="验证码"');
});
