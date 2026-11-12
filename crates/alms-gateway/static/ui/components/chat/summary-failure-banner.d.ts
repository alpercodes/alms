import type { ComponentChild } from "preact";

export interface SummaryFailureBannerProps {
  sessionId: string | null;
  onOpenSettings: () => void;
}

export function SummaryFailureBanner(props: SummaryFailureBannerProps): ComponentChild;
