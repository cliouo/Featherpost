use maild_translate::{detect_language, DetectedLanguage};

#[test]
fn detects_major_languages_accurately() {
    assert_eq!(
        detect_language(
            "请查看本周的发布计划，我们需要在今天下午确认所有测试结果和部署时间。",
            Some("本周发布计划确认"),
        )
        .language,
        DetectedLanguage::ZhHans
    );

    assert_eq!(
        detect_language(
            "請確認這份專案進度報告，我們將在明天上午進行系統更新與測試驗證，謝謝您的協助。",
            Some("專案進度通知"),
        )
        .language,
        DetectedLanguage::ZhHant
    );

    assert_eq!(
        detect_language(
            "いつもお世話になっております。明日の会議の資料を添付いたしますので、ご確認をお願いいたします。",
            Some("明日の会議について"),
        )
        .language,
        DetectedLanguage::Ja
    );

    assert_eq!(
        detect_language(
            "안녕하세요, 요청하신 프로젝트 일정과 회의 자료를 공유드립니다. 확인 후 회신 부탁드립니다.",
            None,
        )
        .language,
        DetectedLanguage::Ko
    );

    assert_eq!(
        detect_language(
            "Hello team, please review the updated release notes and let us know if you have any questions about the schedule.",
            Some("Release schedule update"),
        )
        .language,
        DetectedLanguage::En
    );

    assert_eq!(
        detect_language(
            "Bonjour, veuillez trouver ci-joint le compte rendu de notre réunion. Merci de nous confirmer votre disponibilité pour la semaine prochaine.",
            None,
        )
        .language,
        DetectedLanguage::Fr
    );

    assert_eq!(
        detect_language(
            "Hallo zusammen, bitte prüfen Sie die aktualisierten Unterlagen für das Treffen morgen und geben Sie uns eine kurze Rückmeldung.",
            None,
        )
        .language,
        DetectedLanguage::De
    );

    assert_eq!(
        detect_language(
            "Hola equipo, por favor revisen el documento adjunto con los detalles del proyecto y confirmen su asistencia para la reunión.",
            None,
        )
        .language,
        DetectedLanguage::Es
    );

    assert_eq!(
        detect_language(
            "Olá equipe, por favor revisem a atualização do cronograma e confirmem se todos os requisitos estão prontos para a entrega.",
            None,
        )
        .language,
        DetectedLanguage::Pt
    );

    assert_eq!(
        detect_language(
            "Ciao a tutti, grazie per il vostro aggiornamento. Vi prego di verificare i dettagli del progetto prima della riunione di domani.",
            None,
        )
        .language,
        DetectedLanguage::It
    );

    assert_eq!(
        detect_language(
            "Hallo allemaal, bedankt voor jullie bericht. Kunnen jullie voor morgen het voorstel bekijken en laten weten of alles akkoord is?",
            None,
        )
        .language,
        DetectedLanguage::Nl
    );

    assert_eq!(
        detect_language(
            "Здравствуйте! Пожалуйста, посмотрите обновлённый отчёт по проекту и сообщите, если у вас есть вопросы по срокам.",
            None,
        )
        .language,
        DetectedLanguage::Ru
    );

    assert_eq!(
        detect_language(
            "Вітаю! Будь ласка, перегляньте оновлений звіт щодо проєкту та повідомте, якщо у вас є запитання щодо графіка.",
            None,
        )
        .language,
        DetectedLanguage::Uk
    );

    assert_eq!(
        detect_language(
            "مرحبا، يرجى مراجعة التقرير المحدث للمشروع وإبلاغنا إذا كانت لديكم أي أسئلة حول الجدول الزمني القادم.",
            None,
        )
        .language,
        DetectedLanguage::Ar
    );

    assert_eq!(
        detect_language(
            "สวัสดีครับ กรุณาตรวจสอบเอกสารการประชุมสำหรับวันพรุ่งนี้และแจ้งกลับหากมีข้อสงสัยเพิ่มเติมครับ",
            None,
        )
        .language,
        DetectedLanguage::Th
    );

    assert_eq!(
        detect_language(
            "Γεια σας, παρακαλούμε ελέγξτε την ενημερωμένη αναφορά του έργου και ενημερώστε μας αν έχετε ερωτήσεις.",
            None,
        )
        .language,
        DetectedLanguage::El
    );

    assert_eq!(
        detect_language(
            "שלום לכולם, אנא בדקו את הדוח המעודכן של הפרויקט ועדכנו אותנו אם יש לכם שאלות נוספות.",
            None,
        )
        .language,
        DetectedLanguage::He
    );

    assert_eq!(
        detect_language(
            "नमस्ते, कृपया परियोजना की अद्यतन रिपोर्ट की समीक्षा करें और यदि आपका कोई प्रश्न है तो हमें बताएं।",
            None,
        )
        .language,
        DetectedLanguage::Hi
    );
}

#[test]
fn fail_quiet_fixtures_short_otp_kanji_only_mixed_cjk_en_diff_and_non_russian_cyrillic() {
    // 1. Short subject / <16 effective chars -> Unknown.
    assert_eq!(detect_language("", Some("Hi!")).language, DetectedLanguage::Unknown);
    assert_eq!(detect_language("OK, thanks", None).language, DetectedLanguage::Unknown);

    // 2. OTP / verification code -> Unknown.
    assert_eq!(
        detect_language("Your verification code is 482910.", Some("OTP 482910")).language,
        DetectedLanguage::Unknown
    );
    assert_eq!(
        detect_language("您的验证码 839201", None).language,
        DetectedLanguage::Unknown
    );

    // 3. Kanji-only Japanese ambiguity -> Unknown (must NOT misclassify as Chinese).
    assert_eq!(
        detect_language("東京都渋谷区駅前再開発計画報告書提出済確認依頼", None).language,
        DetectedLanguage::Unknown
    );

    // 4. Balanced mixed CJK/English technical email -> Unknown.
    assert_eq!(
        detect_language(
            "请确认今天的 release checklist，然后 we will deploy the updated build to staging and verify that all integration tests pass before tomorrow.",
            None,
        )
        .language,
        DetectedLanguage::Unknown
    );
    // Primarily Chinese technical email with English product terms still detects as ZhHans.
    assert_eq!(
        detect_language(
            "请查看 GitHub PR 并确认今天的 API 部署计划，我们需要在发版前核对所有测试用例。",
            None,
        )
        .language,
        DetectedLanguage::ZhHans
    );

    // 5. GitHub diff notification -> Unknown.
    let diff_email = r#"
diff --git a/crates/maild-translate/src/lib.rs b/crates/maild-translate/src/lib.rs
index 1234567..89abcde 100644
--- a/crates/maild-translate/src/lib.rs
+++ b/crates/maild-translate/src/lib.rs
@@ -1,5 +1,7 @@
 pub fn hello() {
-    println!("old");
+    println!("new");
 }
"#;
    assert_eq!(detect_language(diff_email, Some("[GitHub] PR #42")).language, DetectedLanguage::Unknown);

    // 6. Serbian Cyrillic (non-Russian/Ukrainian) -> Unknown (Cyrillic alone cannot force Russian).
    assert_eq!(
        detect_language(
            "Поштовани, молимо вас да прегледате извештај за људе у тиму и потврдите долазак сутра.",
            None,
        )
        .language,
        DetectedLanguage::Unknown
    );

    // 7. HTML-only email text strips tags/styles cleanly and detects prose language.
    let html_only = "<html><head><style>.header { font-size: 14px; color: #333; }</style></head>\
                     <body><div><p>Hello everyone, please review the quarterly report attached to this email and reply with your feedback.</p></div></body></html>";
    assert_eq!(detect_language(html_only, None).language, DetectedLanguage::En);
}

#[test]
fn zh_script_and_region_matching_and_ignore_rules() {
    // Target `"zh"` matches both Simplified and Traditional.
    assert!(DetectedLanguage::ZhHans.matches_target("zh"));
    assert!(DetectedLanguage::ZhHant.matches_target("zh"));

    // Regional / script-specific tags distinguish Simplified vs Traditional.
    assert!(DetectedLanguage::ZhHans.matches_target("zh-CN"));
    assert!(DetectedLanguage::ZhHans.matches_target("zh_Hans"));
    assert!(!DetectedLanguage::ZhHans.matches_target("zh-TW"));

    assert!(DetectedLanguage::ZhHant.matches_target("zh-TW"));
    assert!(DetectedLanguage::ZhHant.matches_target("zh-HK"));
    assert!(DetectedLanguage::ZhHant.matches_target("zh-Hant"));
    assert!(!DetectedLanguage::ZhHant.matches_target("zh-CN"));

    // Ignore rules.
    assert!(DetectedLanguage::ZhHans.is_ignored(&["zh".into()]));
    assert!(DetectedLanguage::ZhHant.is_ignored(&["zh".into()]));
    assert!(!DetectedLanguage::ZhHant.is_ignored(&["zh-CN".into()]));
    assert!(DetectedLanguage::En.is_ignored(&["en-US".into()]));
    assert!(!DetectedLanguage::Unknown.is_ignored(&["en".into(), "zh".into()]));
}
