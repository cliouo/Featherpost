import { createContext, useContext, type AriaAttributes, type ComponentType, type ReactNode } from "react";

export interface ChoiceProps extends AriaAttributes {
  value: string;
  onValueChange(value: string): void;
  children: ReactNode;
  disabled?: boolean;
  id?: string;
  className?: string;
  title?: string;
}

// All shared dropdowns use this boundary: Studio supplies a themed picker,
// while the recovery frontend keeps its native select and wire values.
export const ChoiceRenderer = createContext<ComponentType<ChoiceProps> | null>(null);

export function Choice(props: ChoiceProps) {
  const Renderer = useContext(ChoiceRenderer);
  if (Renderer) return <Renderer {...props} />;
  const { onValueChange, ...native } = props;
  return <select {...native} onChange={(event) => onValueChange(event.target.value)} />;
}
