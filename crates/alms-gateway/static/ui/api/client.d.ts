export interface ApiFetchOptions extends RequestInit {
  body?: object;
}

export declare function apiFetch(path: string, opts?: ApiFetchOptions): Promise<unknown>;
export declare function get(path: string): Promise<unknown>;
export declare function post(path: string, body: object): Promise<unknown>;
export declare function put(path: string, body: object): Promise<unknown>;
export declare function patch(path: string, body: object): Promise<unknown>;
export declare function del(path: string): Promise<unknown>;
