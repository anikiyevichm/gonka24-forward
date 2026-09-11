import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.util.concurrent.TimeUnit
import kotlin.system.measureTimeMillis

class MarketplaceHarnessProcessTests {
    @Test
    fun `timeout kills a process whose output remains open`() {
        val process = hangingProcess()

        val elapsedMillis = measureTimeMillis {
            val error = assertThrows(IllegalStateException::class.java) {
                waitForMarketplaceHarnessProcess(
                    process = process,
                    phase = "hanging-regression",
                    timeout = 200,
                    timeoutUnit = TimeUnit.MILLISECONDS,
                )
            }
            assertTrue(error.message.orEmpty().contains("timed out: hanging-regression"))
        }

        assertFalse(process.isAlive, "timed-out process must be dead before the runner returns")
        assertTrue(elapsedMillis < 5_000, "timeout path took ${elapsedMillis}ms")
    }

    private fun hangingProcess(): Process {
        val windows = System.getProperty("os.name").startsWith("Windows", ignoreCase = true)
        val command = if (windows) {
            listOf(
                "powershell.exe",
                "-NoProfile",
                "-Command",
                "Write-Output started; Start-Sleep -Seconds 30",
            )
        } else {
            listOf("/bin/sh", "-c", "printf started; exec sleep 30")
        }
        return ProcessBuilder(command).redirectErrorStream(true).start()
    }
}
