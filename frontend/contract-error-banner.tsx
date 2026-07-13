import { render } from "preact";

export interface ContractErrorBannerProps {
  message: string;
}

export function ContractErrorBanner({ message }: ContractErrorBannerProps) {
  return (
    <div class="contract-error-banner" role="alert">
      <strong>Live data rejected</strong>
      <span>{message}</span>
    </div>
  );
}

let bannerHost: HTMLDivElement | null = null;

export function showContractError(message: string): void {
  if (bannerHost === null || !bannerHost.isConnected) {
    bannerHost = document.createElement("div");
    bannerHost.dataset.almsContractBoundary = "true";
    document.body.prepend(bannerHost);
  }
  render(<ContractErrorBanner message={message} />, bannerHost);
}
