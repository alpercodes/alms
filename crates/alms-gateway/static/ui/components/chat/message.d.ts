import type { VNode } from "preact";

export declare function Message(props: {
  type: string;
  role?: string;
  text?: string;
  sealed?: boolean;
  fromAgent?: string | null;
  reasoning?: string | null;
  ts?: string | null;
}): VNode;

export declare function ThinkingMessage(props: {
  pending?: boolean;
  queuedBehind?: number;
  source?: string | null;
}): VNode;

export declare function ImageMessage(props: {
  role?: string;
  fromAgent?: string | null;
  ts?: string | null;
  url?: string | null;
  alt?: string | null;
}): VNode;
