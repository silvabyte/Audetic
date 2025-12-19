import { Log } from "./logger";

/**
 * Distributed tracing for performance monitoring
 * Tracks operation timing and creates spans for detailed performance analysis
 */

export namespace Tracing {
	interface Span {
		trace_id: string;
		span_id: string;
		parent_span_id?: string;
		operation: string;
		start_time: number;
		end_time?: number;
		duration_ms?: number;
		status?: "success" | "failure";
		metadata?: Record<string, unknown>;
		error?: string;
	}

	const activeSpans = new Map<string, Span>();

	export function startTrace(operation: string) {
		const traceId = Bun.randomUUIDv7();

		const spanId = startSpan({
			operation,
			traceId,
		});

		return { spanId, traceId };
	}

	interface StartSpanArg {
		operation: string;
		traceId?: string;
		parentSpanId?: string;
		metadata?: Record<string, unknown>;
	}
	export function startSpan({
		operation,
		metadata,
		traceId,
		parentSpanId,
	}: StartSpanArg): string {
		//
		const spanId = Bun.randomUUIDv7();
		traceId = traceId ?? Bun.randomUUIDv7();

		const span: Span = {
			metadata,
			operation,
			parent_span_id: parentSpanId ?? undefined,
			span_id: spanId,
			start_time: Date.now(),
			trace_id: traceId,
		};

		activeSpans.set(spanId, span);

		Log.info(
			{
				metadata,
				operation,
				parent_span_id: parentSpanId,
				span_id: spanId,
				status: "started",
				trace_id: traceId,
			},
			`[SPAN] ${operation} span started`,
		);

		return spanId;
	}

	interface EndSpanArg {
		//
		spanId: string;
		status: "success" | "failure";
		error?: string;
	}
	export function endSpan({ spanId, status, error }: EndSpanArg): void {
		const span = activeSpans.get(spanId);
		if (!span) {
			Log.warn({ span_id: spanId }, "[TRACE] Span not found");
			return;
		}

		span.end_time = Date.now();
		span.duration_ms = span.end_time - span.start_time;
		span.status = status;
		span.error = error;

		// Log the completed span
		Log.info(
			{
				duration_ms: span.duration_ms,
				error: span.error,
				metadata: span.metadata,
				operation: span.operation,
				parent_span_id: span.parent_span_id,
				span_id: span.span_id,
				status: span.status,
				trace_id: span.trace_id,
			},
			`[SPAN] ${span.operation} span completed`,
		);

		// Clean up
		activeSpans.delete(spanId);
	}

	export async function trace<T>(
		operation: string,
		fn: () => Promise<T>,
		metadata?: Record<string, unknown>,
	): Promise<T> {
		const spanId = startSpan({ metadata, operation });

		try {
			const result = await fn();
			endSpan({
				spanId,
				status: "success",
			});
			return result;
		} catch (error) {
			endSpan({
				error: error instanceof Error ? error.message : String(error),
				spanId,
				status: "failure",
			});
			throw error;
		}
	}

	export function endTrace(traceId: string): void {
		// Clean up any remaining spans for this trace
		if (traceId) {
			for (const [spanId, span] of activeSpans.entries()) {
				if (span.trace_id === traceId) {
					endSpan({
						spanId,
						status: "success",
					});
				}
			}
		} else {
			Log.warn({ trace_id: traceId }, "[TRACE] Trace not found");
		}
	}
}
