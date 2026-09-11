package com.productscience

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import java.nio.file.Files

class DockerBindUserArgsTests {
    @Test
    fun `docker bind ownership follows the current filesystem user when unix attributes exist`() {
        val directory = Files.createTempDirectory("docker-bind-owner")
        try {
            val uid = runCatching { Files.getAttribute(directory, "unix:uid") }.getOrNull()
            val gid = runCatching { Files.getAttribute(directory, "unix:gid") }.getOrNull()

            val expected = if (uid != null && gid != null) "$uid:$gid" else null
            assertEquals(expected, dockerBindOwner(directory))
        } finally {
            Files.deleteIfExists(directory)
        }
    }
}
