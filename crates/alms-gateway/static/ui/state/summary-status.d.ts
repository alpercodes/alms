export interface SummaryFailure {
  sessionId: string;
  code: string;
  provider: string;
  model: string;
  message: string;
}

export interface SummaryStatusPayload {
  session_id: string;
  failure: { code: string; provider: string; model: string; message: string } | null;
}

export function applySummaryStatus(data: SummaryStatusPayload): void;
export function clearSummaryFailure(): void;
export function dismissSummaryFailure(): void;
export function visibleSummaryFailure(sessionId: string | null): SummaryFailure | null;
