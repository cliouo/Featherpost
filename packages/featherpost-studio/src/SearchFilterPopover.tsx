import { m } from "@featherpost/i18n/messages";
import {
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";

import { Icon } from "./app/Icon";
import { useUiLocale } from "./i18n";
import type { MailboxQuery } from "./mailbox/types";
import {
  availableScopeTiers,
  defaultScopeTier,
  emptyFilterDraft,
  fromMailboxQuery,
  validateSearchFilterDraft,
  type DatePreset,
  type SearchFilterDraft,
  type SearchFilterMeta,
  type SearchScopeContext,
  type SearchScopeTier,
} from "./searchFilters";
import {
  StudioPopover,
  StudioPopoverClose,
  Switch,
} from "./ui";

export interface SearchFilterPopoverProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Applied search query (when currently in search scope). */
  appliedQuery?: MailboxQuery;
  /** Applied frontend-only search metadata. */
  appliedMeta?: SearchFilterMeta;
  /** Active scope tier synchronized with the topbar Choice. */
  scopeTier: SearchScopeTier;
  /** Base mailbox context for resolving 3-tier scope options. */
  scopeContext: SearchScopeContext;
  /** Human-readable scope tooltips keyed by tier. */
  scopeHints?: Partial<Record<SearchScopeTier, string>>;
  /** Count of active structured filters shown on the trigger badge. */
  activeCount: number;
  /** Suppress focus return to trigger when closing to hand off to a confirm dialog. */
  focusHandoff?: boolean;
  /** Called only when the user clicks Apply with a valid draft. */
  onApply: (draft: SearchFilterDraft) => void;
  container?: HTMLElement | null;
}

const DATE_PRESETS: readonly DatePreset[] = [
  "any",
  "7d",
  "30d",
  "90d",
  "custom",
];

function datePresetLabel(preset: DatePreset): string {
  switch (preset) {
    case "any":
      return m.studio_search_filter_date_any();
    case "7d":
      return m.studio_search_filter_date_7d();
    case "30d":
      return m.studio_search_filter_date_30d();
    case "90d":
      return m.studio_search_filter_date_90d();
    case "custom":
      return m.studio_search_filter_date_custom();
  }
}

function scopeTierLabel(tier: SearchScopeTier): string {
  switch (tier) {
    case "folder":
      return m.studio_search_filter_scope_folder();
    case "account":
      return m.studio_search_filter_scope_account();
    case "group":
      return m.studio_search_filter_scope_group();
    case "all":
      return m.studio_search_filter_scope_all();
  }
}

export function SearchFilterPopover({
  open,
  onOpenChange,
  appliedQuery,
  appliedMeta,
  scopeTier,
  scopeContext,
  scopeHints,
  activeCount,
  focusHandoff = false,
  onApply,
  container,
}: SearchFilterPopoverProps) {
  useUiLocale();
  const baseId = useId();
  const fromId = `${baseId}-from`;
  const toId = `${baseId}-to`;
  const subjectId = `${baseId}-subject`;
  const unreadId = `${baseId}-unread`;
  const flaggedId = `${baseId}-flagged`;
  const attachmentsId = `${baseId}-attachments`;
  const startDateId = `${baseId}-start-date`;
  const endDateId = `${baseId}-end-date`;
  const errorId = `${baseId}-error`;
  const dateLegendId = `${baseId}-date-legend`;
  const scopeLegendId = `${baseId}-scope-legend`;

  const fromRef = useRef<HTMLInputElement>(null);
  const toRef = useRef<HTMLInputElement>(null);
  const subjectRef = useRef<HTMLInputElement>(null);
  const startDateRef = useRef<HTMLInputElement>(null);
  const endDateRef = useRef<HTMLInputElement>(null);

  const [draft, setDraft] = useState<SearchFilterDraft>(() =>
    fromMailboxQuery(appliedQuery, appliedMeta, scopeTier),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const [invalidField, setInvalidField] = useState<
    "from" | "to" | "subject" | "startDate" | "endDate" | undefined
  >(undefined);

  // Re-hydrate the draft from the applied search state every time the popover opens.
  // Editing the draft while open never navigates or fires RPCs.
  useEffect(() => {
    if (open) {
      setDraft(fromMailboxQuery(appliedQuery, appliedMeta, scopeTier));
      setValidationError(null);
      setInvalidField(undefined);
    }
  }, [open, appliedQuery, appliedMeta, scopeTier]);

  const tiers = availableScopeTiers(scopeContext);
  const triggerLabel =
    activeCount > 0
      ? m.studio_search_filter_open_active({ count: activeCount })
      : m.studio_search_filter_open();

  const clearError = () => {
    if (validationError !== null) {
      setValidationError(null);
      setInvalidField(undefined);
    }
  };

  const focusInvalidField = (
    field?: "from" | "to" | "subject" | "startDate" | "endDate",
  ) => {
    switch (field) {
      case "from":
        fromRef.current?.focus();
        break;
      case "to":
        toRef.current?.focus();
        break;
      case "subject":
        subjectRef.current?.focus();
        break;
      case "startDate":
        startDateRef.current?.focus();
        break;
      case "endDate":
        endDateRef.current?.focus();
        break;
    }
  };

  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    // Stop React synthetic submit event from bubbling through the portal to the outer .studio-search <form>.
    event.stopPropagation();
    const validation = validateSearchFilterDraft(draft);
    if (!validation.valid) {
      setValidationError(validation.error);
      setInvalidField(validation.invalidField);
      focusInvalidField(validation.invalidField);
      return;
    }
    setValidationError(null);
    setInvalidField(undefined);
    onApply(draft);
  };

  const handleInputKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (
      event.key === "Enter" &&
      (event.nativeEvent.isComposing || event.keyCode === 229)
    ) {
      event.preventDefault();
    }
  };

  const handleReset = () => {
    setDraft(emptyFilterDraft(defaultScopeTier(scopeContext)));
    setValidationError(null);
    setInvalidField(undefined);
  };

  return (
    <StudioPopover
      open={open}
      onOpenChange={onOpenChange}
      align="end"
      side="bottom"
      sideOffset={8}
      collisionPadding={12}
      contentClassName="search-filter-popover"
      title={m.studio_search_filter_title()}
      finalFocus={focusHandoff ? "none" : undefined}
      container={container}
      trigger={
        <button
          type="button"
          className={`search-filter-trigger${activeCount > 0 ? " is-active" : ""}`}
          data-tauri-drag-region="false"
          title={triggerLabel}
          aria-label={triggerLabel}
        >
          <Icon name="sliders" />
          {activeCount > 0 && (
            <span className="search-filter-badge" aria-hidden="true">
              {activeCount}
            </span>
          )}
        </button>
      }
    >
      <StudioPopoverClose
        type="button"
        className="icon-button search-filter-close"
        title={m.studio_search_filter_close()}
        aria-label={m.studio_search_filter_close()}
      >
        ×
      </StudioPopoverClose>
      <form
        className="search-filter-form"
        noValidate
        onSubmit={handleSubmit}
      >
        <div className="search-filter-fields">
          <div className="search-filter-field">
            <label htmlFor={fromId}>{m.studio_search_filter_from()}</label>
            <input
              ref={fromRef}
              id={fromId}
              name="from"
              type="text"
              autoCorrect="off"
              autoCapitalize="none"
              spellCheck={false}
              enterKeyHint="search"
              placeholder={m.studio_search_filter_from_placeholder()}
              value={draft.from}
              aria-invalid={invalidField === "from" ? "true" : "false"}
              aria-describedby={invalidField === "from" ? errorId : undefined}
              onKeyDown={handleInputKeyDown}
              onChange={(e) => {
                clearError();
                setDraft((prev) => ({ ...prev, from: e.target.value }));
              }}
            />
          </div>
          <div className="search-filter-field">
            <label htmlFor={toId}>{m.studio_search_filter_to()}</label>
            <input
              ref={toRef}
              id={toId}
              name="to"
              type="text"
              autoCorrect="off"
              autoCapitalize="none"
              spellCheck={false}
              enterKeyHint="search"
              placeholder={m.studio_search_filter_to_placeholder()}
              value={draft.to}
              aria-invalid={invalidField === "to" ? "true" : "false"}
              aria-describedby={invalidField === "to" ? errorId : undefined}
              onKeyDown={handleInputKeyDown}
              onChange={(e) => {
                clearError();
                setDraft((prev) => ({ ...prev, to: e.target.value }));
              }}
            />
          </div>
          <div className="search-filter-field">
            <label htmlFor={subjectId}>{m.studio_search_filter_subject()}</label>
            <input
              ref={subjectRef}
              id={subjectId}
              name="subject"
              type="text"
              enterKeyHint="search"
              placeholder={m.studio_search_filter_subject_placeholder()}
              value={draft.subject}
              aria-invalid={invalidField === "subject" ? "true" : "false"}
              aria-describedby={invalidField === "subject" ? errorId : undefined}
              onKeyDown={handleInputKeyDown}
              onChange={(e) => {
                clearError();
                setDraft((prev) => ({ ...prev, subject: e.target.value }));
              }}
            />
          </div>
        </div>

        <div className="search-filter-toggles">
          <div className="search-filter-toggle-row">
            <label htmlFor={unreadId}>{m.studio_search_filter_unread()}</label>
            <Switch
              id={unreadId}
              checked={draft.unreadOnly}
              onCheckedChange={(checked) =>
                setDraft((prev) => ({ ...prev, unreadOnly: checked }))
              }
            />
          </div>
          <div className="search-filter-toggle-row">
            <label htmlFor={flaggedId}>{m.studio_search_filter_flagged()}</label>
            <Switch
              id={flaggedId}
              checked={draft.flaggedOnly}
              onCheckedChange={(checked) =>
                setDraft((prev) => ({ ...prev, flaggedOnly: checked }))
              }
            />
          </div>
          <div className="search-filter-toggle-row">
            <label htmlFor={attachmentsId}>
              {m.studio_search_filter_attachments()}
            </label>
            <Switch
              id={attachmentsId}
              checked={draft.hasAttachments}
              onCheckedChange={(checked) =>
                setDraft((prev) => ({ ...prev, hasAttachments: checked }))
              }
            />
          </div>
        </div>

        <fieldset className="search-filter-section">
          <legend id={dateLegendId}>
            {m.studio_search_filter_date_label()}
          </legend>
          <div
            className="search-filter-segmented"
            role="group"
            aria-labelledby={dateLegendId}
          >
            {DATE_PRESETS.map((preset) => (
              <button
                key={preset}
                type="button"
                className={draft.datePreset === preset ? "is-selected" : undefined}
                aria-pressed={draft.datePreset === preset}
                onClick={() => {
                  clearError();
                  setDraft((prev) => ({ ...prev, datePreset: preset }));
                }}
              >
                {datePresetLabel(preset)}
              </button>
            ))}
          </div>
          {draft.datePreset === "custom" && (
            <div className="search-filter-date-inputs">
              <div className="search-filter-field">
                <label htmlFor={startDateId}>
                  {m.studio_search_filter_date_start()}
                </label>
                <input
                  ref={startDateRef}
                  id={startDateId}
                  name="startDate"
                  type="date"
                  value={draft.startDate}
                  aria-invalid={invalidField === "startDate" ? "true" : "false"}
                  aria-describedby={
                    invalidField === "startDate" || invalidField === "endDate"
                      ? errorId
                      : undefined
                  }
                  onKeyDown={handleInputKeyDown}
                  onChange={(e) => {
                    clearError();
                    setDraft((prev) => ({ ...prev, startDate: e.target.value }));
                  }}
                />
              </div>
              <div className="search-filter-field">
                <label htmlFor={endDateId}>
                  {m.studio_search_filter_date_end()}
                </label>
                <input
                  ref={endDateRef}
                  id={endDateId}
                  name="endDate"
                  type="date"
                  value={draft.endDate}
                  aria-invalid={invalidField === "endDate" ? "true" : "false"}
                  aria-describedby={
                    invalidField === "startDate" || invalidField === "endDate"
                      ? errorId
                      : undefined
                  }
                  onKeyDown={handleInputKeyDown}
                  onChange={(e) => {
                    clearError();
                    setDraft((prev) => ({ ...prev, endDate: e.target.value }));
                  }}
                />
              </div>
            </div>
          )}
        </fieldset>

        {validationError && (
          <div id={errorId} className="search-filter-error" role="alert">
            {validationError}
          </div>
        )}

        <fieldset className="search-filter-section">
          <legend id={scopeLegendId}>
            {m.studio_search_filter_scope_label()}
          </legend>
          <div
            className="search-filter-segmented"
            role="group"
            aria-labelledby={scopeLegendId}
          >
            {tiers.map((tier) => (
              <button
                key={tier}
                type="button"
                className={draft.scope === tier ? "is-selected" : undefined}
                aria-pressed={draft.scope === tier}
                title={scopeHints?.[tier]}
                onClick={() => setDraft((prev) => ({ ...prev, scope: tier }))}
              >
                {scopeTierLabel(tier)}
              </button>
            ))}
          </div>
        </fieldset>

        <footer className="search-filter-footer">
          <button
            type="button"
            className="text-button search-filter-reset"
            onClick={handleReset}
          >
            {m.studio_search_filter_reset()}
          </button>
          <div className="search-filter-footer-actions">
            <button
              type="button"
              className="secondary-button"
              onClick={() => onOpenChange(false)}
            >
              {m.studio_search_filter_cancel()}
            </button>
            <button type="submit" className="primary-button">
              {m.studio_search_filter_apply()}
            </button>
          </div>
        </footer>
      </form>
    </StudioPopover>
  );
}
