import java.io.File
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

internal fun runMarketplaceHarnessProcess(
    command: List<String>,
    directory: File,
    phase: String?,
    timeout: Long,
    timeoutUnit: TimeUnit,
): String {
    val process = ProcessBuilder(command)
        .directory(directory)
        .redirectErrorStream(true)
        .start()

    return waitForMarketplaceHarnessProcess(process, phase, timeout, timeoutUnit)
}

internal fun waitForMarketplaceHarnessProcess(
    process: Process,
    phase: String?,
    timeout: Long,
    timeoutUnit: TimeUnit,
): String {
    val output = StringBuffer()
    val readFailure = AtomicReference<Throwable?>()
    val outputReader = Thread({
        try {
            process.inputStream.bufferedReader().use { reader ->
                val buffer = CharArray(DEFAULT_BUFFER_SIZE)
                while (true) {
                    val count = reader.read(buffer)
                    if (count < 0) break
                    output.append(buffer, 0, count)
                }
            }
        } catch (error: Throwable) {
            readFailure.set(error)
        }
    }, "marketplace-harness-output-${phase ?: "unknown"}").apply {
        isDaemon = true
        start()
    }

    val completed = try {
        process.waitFor(timeout, timeoutUnit)
    } catch (error: InterruptedException) {
        stopProcess(process)
        Thread.currentThread().interrupt()
        throw IllegalStateException("Interrupted while waiting for Marketplace harness: $phase", error)
    }

    if (!completed) {
        stopProcess(process)
        outputReader.join(TimeUnit.SECONDS.toMillis(5))
        check(!outputReader.isAlive) {
            "Marketplace harness output reader did not stop after process termination: $phase"
        }
        throw IllegalStateException("Marketplace harness timed out: $phase\n$output")
    }

    outputReader.join(TimeUnit.SECONDS.toMillis(5))
    check(!outputReader.isAlive) {
        "Marketplace harness output reader did not finish: $phase"
    }
    readFailure.get()?.let { error ->
        throw IllegalStateException("Failed to read Marketplace harness output: $phase", error)
    }
    check(process.exitValue() == 0) {
        "Marketplace harness failed ($phase):\n$output"
    }
    return output.toString()
}

private fun stopProcess(process: Process) {
    try {
        process.destroyForcibly()
        check(process.waitFor(5, TimeUnit.SECONDS)) {
            "Marketplace harness process did not stop after destroyForcibly()"
        }
    } finally {
        runCatching { process.outputStream.close() }
        runCatching { process.inputStream.close() }
        runCatching { process.errorStream.close() }
    }
}
