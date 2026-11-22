import { createContext, useContext, type ComponentType, type ReactNode } from "react";

export interface ChoiceProps {
  value: string;
  onValueChange(value: string): void;
  children: ReactNode;
  disabled?: boolean;
  id?: string;
  className?: string;
  title?: string;
  "aria-label"?: string;
  "aria-labelledby"?: string;
  "aria-describedby"?: string;
}

// Studio supplies a themed picker. Legacy keeps its native select and wire values.
export const ChoiceRenderer = createContext<ComponentType<ChoiceProps> | null>(null);

export function Choice(props: ChoiceProps) {
  const Renderer = useContext(ChoiceRenderer);
  if (Renderer) return <Renderer {...props} />;
  const { onValueChange, ...native } = props;
  return <select {...native} onChange={(event) => onValueChange(event.target.value)} />;
}
