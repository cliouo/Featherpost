import { m } from "@featherpost/i18n/messages";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { UiContactSuggestion } from "../bridge/types";
import { useStore } from "../host/hooks";
import { useUiLocale } from "../i18n";
import {
  TokenInput,
  type TokenInputItem,
  type TokenInputSuggestion,
} from "../ui";
import type { ComposeController } from "./controller";
import {
  hasTopLevelDelimiter,
  isDelimiterKeyAtEnd,
  isValidAddrSpec,
  recipientDedupKey,
  type RecipientFieldName,
} from "./recipients";

export interface RecipientFieldProps {
  controller: ComposeController;
  field: RecipientFieldName;
  label: string;
  placeholder: string;
  disabled?: boolean;
  error?: string;
  debounceMs?: number;
}

const DEFAULT_DEBOUNCE_MS = 120;
const SUGGESTION_LIMIT = 8;

export function RecipientField({
  controller,
  field,
  label,
  placeholder,
  disabled = false,
  error,
  debounceMs = DEFAULT_DEBOUNCE_MS,
}: RecipientFieldProps) {
  useUiLocale();
  const s = useStore(controller.store);
  const draftId = s.draft?.id ?? "";
  const account = s.fields?.account ?? "";
  const online = s.online;
  const fieldState = s.recipients?.[field];
  const chips = fieldState?.chips ?? [];
  const pending = fieldState?.pending ?? "";
  const editingIndex = fieldState?.editingIndex ?? null;
  const recipientNames = s.recipientNames;

  const [popupOpen, setPopupOpen] = useState(false);
  const [suggestions, setSuggestions] = useState<UiContactSuggestion[]>([]);
  const [loading, setLoading] = useState(false);

  const mountedRef = useRef(true);
  const queryGenRef = useRef(0);
  const activeKeyRef = useRef("");

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      queryGenRef.current += 1;
      activeKeyRef.current = "";
    };
  }, []);

  // Immediately clear stale suggestions on draft/account/field switch.
  useEffect(() => {
    queryGenRef.current += 1;
    activeKeyRef.current = "";
    setSuggestions([]);
    setLoading(false);
  }, [draftId, account, field]);

  // Debounced async contact search gated by (draftId, account, field, queryGen).
  useEffect(() => {
    if (!popupOpen || disabled || !account) {
      queryGenRef.current += 1;
      activeKeyRef.current = "";
      setLoading(false);
      return;
    }

    const gen = ++queryGenRef.current;
    const requestKey = JSON.stringify([draftId, account, field, gen]);
    activeKeyRef.current = requestKey;
    const delay = pending.trim().length > 0 ? debounceMs : 0;
    setLoading(true);

    const run = () => {
      if (!mountedRef.current || activeKeyRef.current !== requestKey) return;
      void controller
        .searchContacts(account, pending, SUGGESTION_LIMIT)
        .then((result) => {
          if (!mountedRef.current || activeKeyRef.current !== requestKey) return;
          setSuggestions(result);
          setLoading(false);
        })
        .catch(() => {
          if (!mountedRef.current || activeKeyRef.current !== requestKey) return;
          setSuggestions([]);
          setLoading(false);
        });
    };

    if (delay <= 0) {
      run();
      return () => {
        if (activeKeyRef.current === requestKey) {
          activeKeyRef.current = "";
        }
      };
    }

    const timer = setTimeout(run, delay);
    return () => {
      clearTimeout(timer);
      if (activeKeyRef.current === requestKey) {
        activeKeyRef.current = "";
      }
    };
  }, [account, controller, debounceMs, disabled, draftId, field, online, pending, popupOpen]);

  const tokens = useMemo<TokenInputItem[]>(
    () =>
      chips.map((value) => {
        const valid = isValidAddrSpec(value);
        const key = recipientDedupKey(value);
        const displayName = valid ? recipientNames[key] : undefined;
        return {
          value,
          label: displayName ?? value,
          secondary: displayName ? value : undefined,
          title: !valid
            ? m.compose_recipient_invalid_title({ value })
            : displayName
              ? `${displayName} <${value}>`
              : value,
          invalid: !valid,
        };
      }),
    [chips, recipientNames],
  );

  const suggestionItems = useMemo<TokenInputSuggestion[]>(
    () =>
      suggestions.map((item) => ({
        value: item.address,
        label: item.name || item.address,
        secondary: item.name ? item.address : undefined,
      })),
    [suggestions],
  );

  const handleQueryChange = useCallback(
    (next: string) => {
      controller.setRecipientPending(field, next);
    },
    [controller, field],
  );

  const handleCommitText = useCallback(
    (raw: string) => {
      controller.commitRecipientInput(field, raw);
    },
    [controller, field],
  );

  const handlePasteText = useCallback(
    (pasted: string, selectionStart: number, selectionEnd: number) => {
      controller.pasteRecipientText(field, pasted, selectionStart, selectionEnd);
    },
    [controller, field],
  );

  const handleSelectSuggestion = useCallback(
    (item: TokenInputSuggestion) => {
      controller.selectRecipientSuggestion(field, {
        address: item.value,
        name: item.secondary ? item.label : undefined,
      });
    },
    [controller, field],
  );

  const handleRemoveToken = useCallback(
    (index: number) => {
      controller.removeRecipient(field, index);
    },
    [controller, field],
  );

  const handleStartEdit = useCallback(
    (index: number) => {
      controller.startEditRecipient(field, index);
    },
    [controller, field],
  );

  const handleCancelEdit = useCallback(() => {
    controller.cancelEditRecipient(field);
  }, [controller, field]);

  const removeLabel = useCallback(
    (item: TokenInputItem) =>
      m.compose_recipient_remove_label({
        recipient: item.secondary ? `${item.label} <${item.secondary}>` : item.value,
      }),
    [],
  );

  return (
    <div className="compose-field compose-recipient-field">
      <label htmlFor={`compose-${field}`}>{label}</label>
      <div className="compose-control">
        <TokenInput
          id={`compose-${field}`}
          name={field}
          tokens={tokens}
          query={pending}
          onQueryChange={handleQueryChange}
          suggestions={suggestionItems}
          loading={loading}
          disabled={disabled}
          placeholder={placeholder}
          ariaInvalid={Boolean(error)}
          ariaDescribedBy={error ? `error-${field}` : undefined}
          editingIndex={editingIndex}
          onStartEdit={handleStartEdit}
          onCancelEdit={handleCancelEdit}
          onCommitText={handleCommitText}
          onPasteText={handlePasteText}
          onSelectSuggestion={handleSelectSuggestion}
          onRemoveToken={handleRemoveToken}
          onOpenChange={setPopupOpen}
          removeChipLabel={removeLabel}
          removeChipTitle={removeLabel}
          editChipHint={m.compose_recipient_edit_hint()}
          emptyLabel={m.compose_recipient_suggestions_empty()}
          loadingLabel={m.compose_recipient_suggestions_loading()}
          suggestionsLabel={m.compose_recipient_suggestions_label()}
          isDelimiterKey={isDelimiterKeyAtEnd}
          shouldCommitOnCompositionEnd={hasTopLevelDelimiter}
          normalizeKey={recipientDedupKey}
        />
        {error && (
          <span className="field-error" id={`error-${field}`}>
            {error}
          </span>
        )}
      </div>
    </div>
  );
}
