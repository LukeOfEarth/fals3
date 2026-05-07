import type { Fals3 } from './index';

/**
 * SDK command duck-type. Anything with a `constructor.name` matching one of
 * the supported AWS SDK v3 command classes (e.g. `GetObjectCommand`) and an
 * `input` property satisfies this — including the real classes from
 * `@aws-sdk/client-s3`.
 */
export interface SdkCommand<TInput = unknown, TOutput = unknown> {
  readonly input: TInput;
  // Phantom type marker — never read at runtime; helps TypeScript infer.
  readonly __output?: TOutput;
}

export interface Fals3S3ClientOptions {
  /** Defaults to `'fals3y'`. Mostly cosmetic — fals3y doesn't sign anything. */
  region?: string;
  [key: string]: unknown;
}

/**
 * Drop-in replacement for `@aws-sdk/client-s3`'s `S3Client`, backed by a
 * local `Fals3` instance. Dispatches by `command.constructor.name`, so it
 * accepts both the real SDK command classes and any object that mimics
 * their shape.
 *
 * @example
 * ```ts
 * import { Fals3 } from 'fals3y';
 * import { Fals3S3Client } from 'fals3y/sdk-shim';
 * import { GetObjectCommand, PutObjectCommand } from '@aws-sdk/client-s3';
 *
 * const fals3 = Fals3.open({ baseDir: '/tmp/fals3' });
 * const s3 = new Fals3S3Client(fals3);
 *
 * await s3.send(new PutObjectCommand({ Bucket: 'b', Key: 'k', Body: 'hi' }));
 * const out = await s3.send(new GetObjectCommand({ Bucket: 'b', Key: 'k' }));
 * console.log(await out.Body.transformToString());
 * ```
 *
 * **Supported commands:** `CreateBucketCommand`, `HeadBucketCommand`,
 * `DeleteBucketCommand`, `PutObjectCommand`, `GetObjectCommand`,
 * `HeadObjectCommand`, `DeleteObjectCommand`, `ListObjectsV2Command`,
 * `CopyObjectCommand`, `CreateMultipartUploadCommand`, `UploadPartCommand`,
 * `CompleteMultipartUploadCommand`, `AbortMultipartUploadCommand`,
 * `ListPartsCommand`.
 *
 * Errors are rewrapped to match AWS SDK v3 shape: `err.name` carries the
 * AWS error code (e.g. `'NoSuchKey'`), `err.$metadata.httpStatusCode` is
 * populated, and `err.$fault === 'client'`.
 */
export declare class Fals3S3Client {
  constructor(fals3: Fals3, options?: Fals3S3ClientOptions);

  readonly config: Record<string, unknown>;
  readonly middlewareStack: {
    add: (...args: unknown[]) => void;
    remove: (...args: unknown[]) => void;
    use: (...args: unknown[]) => void;
    clone: () => unknown;
  };

  /** Resolve a command and return the SDK-shaped response. */
  send<TInput, TOutput>(
    command: SdkCommand<TInput, TOutput>,
  ): Promise<TOutput & { $metadata: { httpStatusCode: number; requestId: string } }>;

  /** Loose overload for SDK objects whose output type isn't tracked. */
  send(command: { input: unknown }): Promise<any>;

  destroy(): void;

  /**
   * Register or replace a handler for a command class name. Useful for
   * stubbing or extending command coverage in tests.
   */
  static registerCommand(
    name: string,
    handler: (fals3: Fals3, input: any) => unknown,
  ): void;
}
