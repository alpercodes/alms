export interface ApiFetchOptions extends RequestInit {
  body?: object;
}

export declare function apiFetch<T = unknown>(path: string, opts?: ApiFetchOptions): Promise<T>;
export declare function get<T = unknown>(path: string): Promise<T>;
export declare function post<T = unknown>(path: string, body: object): Promise<T>;
export declare function put<T = unknown>(path: string, body: object): Promise<T>;
export declare function patch<T = unknown>(path: string, body: object): Promise<T>;
export declare function del<T = unknown>(path: string): Promise<T>;
